use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::agent::to_openai_tools;
use crate::backends::{
    backend, default_model, validate, BackendDescriptor, BackendName, ProfileName,
};
use crate::batch_operations::{
    execute_batch_operations, find_cross_file_references, find_related_files,
    preview_batch_operations, BatchEditOperation, EditOperation,
};
use crate::budget::{format_bytes, measure_prompt_budget};
use crate::context_guard::{compact_session, context_status_lines, CompactMethod};
use crate::capabilities::{
    self, best_record, recommended_tool_selection, record_score, sorted_records,
    warmup_recommended, BenchmarkStats, CapabilityRecord, CapabilityStatus,
};
use crate::config::{is_tool_name, AgentConfig, OperatorMode, ToolSelection, ALL_TOOL_NAMES};
use crate::handoff::{
    collect_handoff_context, default_export_path as default_handoff_export_path,
    ensure_required_sections, handoff_system_prompt, render_fallback_markdown,
    render_handoff_prompt, should_refuse_cloud_handoff,
};
use crate::hardware::{detect_hardware_spec, save_hardware_summary, HardwareSpec};
use crate::input::plain_read_line;
use crate::openai::{
    list_models, stream_chat, ChatMessage, ChatRequest, StreamOptions, ToolDef, ToolDefFunction,
};
use crate::project_memory::{
    append_project_note, build_project_index, clear_project_index, forget_project_note,
    load_project_index, load_project_notes, project_index_freshness, project_memory_status,
    refresh_changed_project_index, render_repo_map, render_system_prompt_with_memory,
};
use crate::prompt_library::{
    delete_prompt, export_prompts, import_prompts, list_prompts, load_prompt, save_prompt,
    PromptLibrary,
};
use crate::recommend::{
    apply_recommendation_to_config, recommend_models, ModelCandidate, ModelRecommendation,
};
use crate::session::{
    delete_session, list_sessions, load_messages, load_session, render_markdown,
    resolve_session_path, save_message, search_sessions, set_session_title, SessionEntry,
};
use crate::shipcheck::{
    collect_shipcheck, collect_shipcheck_with_tests, default_export_path, file_status_label,
    render_markdown as render_shipcheck_markdown, ShipcheckSnapshot,
};
use crate::test_integration::{
    discover_tests, run_selected_tests, run_tests, smart_test_selection,
};
use crate::tools::{build_tools, build_tools_for_names, select_tool_names};
use crate::warmup::warmup;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";

pub struct AppState {
    pub config: AgentConfig,
    pub http: reqwest::Client,
    pub backend: BackendDescriptor,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub session_dir: String,
    pub session_path: PathBuf,
    pub total_in: u32,
    pub total_out: u32,
    pub context_guard_notice: Option<String>,
}

impl AppState {
    pub fn rebuild_client(&mut self) -> Result<()> {
        let new_backend = backend(self.config.backend);
        validate(&new_backend)?;
        self.backend = new_backend;
        Ok(())
    }
    pub fn resolve_model(&mut self) {
        self.model = default_model(
            &self.backend,
            &self.config.profile,
            self.config.model_override.as_deref(),
            &self.config.profiles,
        );
    }
    pub fn reset_session(&mut self) {
        self.session_path = crate::session::new_session_path(&self.session_dir);
    }
}

pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "List available commands"),
    ("/setup", "Run the setup wizard and write agent.config.json"),
    ("/new", "Start a fresh conversation"),
    ("/clear", "Clear the screen"),
    ("/config", "Show resolved configuration"),
    (
        "/mode",
        "Show or set operator mode (explore, edit, ship, review)",
    ),
    (
        "/shipcheck",
        "Summarize git and project-memory readiness for release",
    ),
    (
        "/handoff",
        "Draft commit, changelog, testing, and X-ready release copy",
    ),
    ("/session", "Show session info and token usage"),
    ("/sessions", "List saved sessions"),
    ("/resume", "Resume latest or named session"),
    ("/export", "Export a session to markdown or json"),
    (
        "/backend",
        "Switch backend (ollama, lm-studio, mlx, llamacpp, openrouter)",
    ),
    (
        "/profile",
        "Switch hardware profile (mac-mini-16gb, mac-studio-32gb)",
    ),
    (
        "/model",
        "List models from the current backend and pick one",
    ),
    (
        "/tools",
        "Show or set enabled tools (comma-separated names)",
    ),
    (
        "/compare",
        "Run the last user prompt against the OpenRouter cloud (requires OPENROUTER_API_KEY)",
    ),
    ("/context", "Show or update context limits and auto-guard status"),
    ("/compact", "Summarize or trim older conversation turns"),
    ("/doctor", "Check backend, tools, env, and session storage"),
    ("/bench", "Measure warmup, first-token, and total latency"),
    ("/capabilities", "Show or refresh model capability cache"),
    (
        "/autotune",
        "Recommend and optionally apply the best cached model",
    ),
    (
        "/recommend",
        "Recommend a model from hardware, installed models, and cached probes",
    ),
    ("/index", "Build, refresh, show, or clear project memory"),
    ("/map", "Print the project memory repo map or focused hits"),
    ("/memory", "Turn project memory on/off or show status"),
    ("/remember", "Save a durable project note"),
    ("/forget", "Remove a project note"),
    ("/eval", "Run prompt/model comparison suite"),
    (
        "/batch",
        "Execute multi-file operations with preview and rollback",
    ),
    ("/refactor", "Find cross-file references and related files"),
    (
        "/test",
        "Discover, run, and analyze tests with smart selection",
    ),
    ("/prompt", "Save, list, run, and manage prompt templates"),
];

fn fmt_tokens(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        n.to_string()
    }
}

pub async fn dispatch(input: &str, state: &mut AppState) -> Result<()> {
    let mut parts = input.splitn(2, ' ');
    let name = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim().to_string();

    match name {
        "/help" => help(),
        "/setup" => cmd_setup(state).await?,
        "/new" => cmd_new(state),
        "/clear" => clear_screen(),
        "/config" => cmd_config(state),
        "/mode" => cmd_mode(&args, state),
        "/shipcheck" => cmd_shipcheck(&args, state)?,
        "/handoff" => cmd_handoff(&args, state).await?,
        "/session" => cmd_session(&args, state)?,
        "/sessions" => cmd_sessions(&args, state)?,
        "/resume" => cmd_resume(&args, state)?,
        "/export" => cmd_export(&args, state)?,
        "/backend" => cmd_backend(&args, state).await?,
        "/profile" => cmd_profile(&args, state).await?,
        "/model" => cmd_model(&args, state).await?,
        "/tools" => cmd_tools(&args, state),
        "/compare" => cmd_compare(&args, state).await?,
        "/context" => cmd_context(&args, state),
        "/compact" => cmd_compact(&args, state).await?,
        "/doctor" => cmd_doctor(&args, state).await?,
        "/bench" => cmd_bench(&args, state).await?,
        "/capabilities" => cmd_capabilities(&args, state).await?,
        "/autotune" => cmd_autotune(&args, state).await?,
        "/recommend" => cmd_recommend(&args, state).await?,
        "/index" => cmd_index(&args, state)?,
        "/map" => cmd_map(&args, state)?,
        "/memory" => cmd_memory(&args, state),
        "/remember" => cmd_remember(&args, state)?,
        "/forget" => cmd_forget(&args, state)?,
        "/eval" => cmd_eval(&args, state).await?,
        "/batch" => cmd_batch(&args, state)?,
        "/refactor" => cmd_refactor(&args, state)?,
        "/test" => cmd_test(&args, state)?,
        "/prompt" => cmd_prompt(&args, state).await?,
        other => {
            println!("  {DIM}Unknown command: {other}. Type /help.{RESET}");
        }
    }
    Ok(())
}

fn help() {
    for (n, d) in COMMANDS {
        println!("  {CYAN}{:<12}{RESET} {DIM}{}{RESET}", n, d);
    }
    println!("  {CYAN}{:<12}{RESET} {DIM}Quit{RESET}", "exit");
}

async fn cmd_setup(state: &mut AppState) -> Result<()> {
    let Some(config) = crate::setup::run_setup_wizard(&state.config).await? else {
        return Ok(());
    };
    let backend_desc = backend(config.backend);
    if let Err(e) = validate(&backend_desc) {
        println!(
            "  {YELLOW}!{RESET} {DIM}Config saved, but active session stayed on the previous backend: {e}{RESET}"
        );
        return Ok(());
    }
    let model = default_model(
        &backend_desc,
        &config.profile,
        config.model_override.as_deref(),
        &config.profiles,
    );
    let old_session_dir = state.session_dir.clone();
    state.config = config;
    state.backend = backend_desc;
    state.model = model;
    state.session_dir = state.config.session_dir.clone();
    if state.session_dir != old_session_dir {
        fs::create_dir_all(&state.session_dir)?;
        state.reset_session();
    }
    println!("  {GREEN}✓{RESET} {DIM}active config updated for this session.{RESET}");
    Ok(())
}

fn cmd_new(state: &mut AppState) {
    state.messages.clear();
    state.reset_session();
    println!("  {GREEN}✓{RESET} {DIM}New session started.{RESET}");
}

fn clear_screen() {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[2J\x1b[H");
    let _ = out.flush();
}

fn cmd_config(state: &AppState) {
    println!(
        "  {DIM}mode{RESET}             {CYAN}{}{RESET}",
        state.config.mode.as_str()
    );
    println!(
        "  {DIM}backend{RESET}          {CYAN}{}{RESET}",
        state.config.backend.as_str()
    );
    println!(
        "  {DIM}profile{RESET}          {CYAN}{}{RESET}",
        state.config.profile
    );
    println!(
        "  {DIM}model{RESET}            {CYAN}{}{RESET}",
        state.model
    );
    println!(
        "  {DIM}workspaceRoot{RESET}    {}",
        state.config.workspace_root
    );
    println!(
        "  {DIM}outsideWorkspace{RESET} {}",
        state.config.outside_workspace.as_str()
    );
    println!(
        "  {DIM}tools{RESET}            {}",
        state.config.tools.join(", ")
    );
    println!(
        "  {DIM}toolSelection{RESET}    {}",
        state.config.tool_selection.as_str()
    );
    println!(
        "  {DIM}slashCommands{RESET}    {}",
        state.config.slash_commands
    );
    println!(
        "  {DIM}showBanner{RESET}       {}",
        state.config.display.show_banner
    );
    println!(
        "  {DIM}context{RESET}          maxMessages={:?} maxBytes={:?}",
        state.config.context.max_messages, state.config.context.max_bytes
    );
    println!(
        "  {DIM}history{RESET}          enabled={} maxEntries={} path={}",
        state.config.history.enabled,
        state.config.history.max_entries,
        state.config.history_path()
    );
    println!(
        "  {DIM}projectMemory{RESET}    enabled={} autoInject={} autoIndex={} maxFileBytes={} maxInjectedBytes={} allowCloudContext={}",
        state.config.project_memory.enabled,
        state.config.project_memory.auto_inject,
        state.config.project_memory.auto_index,
        state.config.project_memory.max_file_bytes,
        state.config.project_memory.max_injected_bytes,
        state.config.project_memory.allow_cloud_context
    );
    if !state.config.profiles.is_empty() {
        println!(
            "  {DIM}customProfiles{RESET}   {}",
            state
                .config
                .profiles
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn cmd_session(args: &str, state: &mut AppState) -> Result<()> {
    let trimmed = args.trim();
    if let Some(title) = trimmed.strip_prefix("title ") {
        set_session_title(&state.session_path, title)?;
        println!(
            "  {GREEN}✓{RESET} {DIM}session title →{RESET} {CYAN}{}{RESET}",
            title.trim()
        );
        return Ok(());
    }
    if !trimmed.is_empty() {
        println!("  {DIM}Usage: /session [title <text>]{RESET}");
        return Ok(());
    }
    println!(
        "  {DIM}mode{RESET}      {CYAN}{}{RESET}",
        state.config.mode.as_str()
    );
    println!(
        "  {DIM}backend{RESET}   {CYAN}{}{RESET}",
        state.config.backend.as_str()
    );
    println!(
        "  {DIM}profile{RESET}   {CYAN}{}{RESET}",
        state.config.profile.as_str()
    );
    println!("  {DIM}model{RESET}     {CYAN}{}{RESET}", state.model);
    println!(
        "  {DIM}approval{RESET}  {CYAN}{}{RESET}",
        state.config.approval_policy.as_str()
    );
    println!("  {DIM}session{RESET}   {}", state.session_path.display());
    println!("  {DIM}messages{RESET}  {}", state.messages.len());
    println!(
        "  {DIM}tokens{RESET}    {} in · {} out",
        fmt_tokens(state.total_in),
        fmt_tokens(state.total_out)
    );
    Ok(())
}

fn cmd_mode(args: &str, state: &mut AppState) {
    let arg = args.trim();
    if arg.is_empty() || arg == "status" {
        println!(
            "  {DIM}mode{RESET}      {CYAN}{}{RESET}",
            state.config.mode.as_str()
        );
        println!("  {DIM}available{RESET} explore, edit, ship, review, custom");
        println!(
            "  {DIM}tools{RESET}     {} · {DIM}toolSelection{RESET} {} · {DIM}approval{RESET} {} · {DIM}maxSteps{RESET} {}",
            state.config.tools.join(", "),
            state.config.tool_selection.as_str(),
            state.config.approval_policy.as_str(),
            state.config.max_steps
        );
        return;
    }
    let Some(mode) = OperatorMode::parse(arg) else {
        println!("  {DIM}Usage: /mode [explore|edit|ship|review|custom]{RESET}");
        return;
    };
    state.config.apply_operator_mode(mode);
    println!(
        "  {GREEN}✓{RESET} {DIM}mode →{RESET} {CYAN}{}{RESET} {DIM}tools={} approval={} maxSteps={}{RESET}",
        state.config.mode.as_str(),
        state.config.tools.join(","),
        state.config.approval_policy.as_str(),
        state.config.max_steps
    );
}

fn cmd_shipcheck(args: &str, state: &AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let action = parts.first().map(|s| *s);
    let export = matches!(action, Some("export") | Some("save"));
    let run_tests = args.contains("--tests");

    if matches!(action, Some(other) if other != "export" && other != "save" && other != "--tests") {
        println!("  {DIM}Usage: /shipcheck [export [path]] [--tests]{RESET}");
        return Ok(());
    }

    // Filter out --tests from path parsing
    let path_parts: Vec<&str> = parts.iter().filter(|p| **p != "--tests").cloned().collect();
    let explicit_path = if path_parts.len() > 1 {
        Some(PathBuf::from(path_parts[1]))
    } else {
        None
    };

    if path_parts.len() > 2 {
        println!("  {DIM}Usage: /shipcheck [export [path]] [--tests]{RESET}");
        return Ok(());
    }

    let snapshot = if run_tests {
        collect_shipcheck_with_tests(&state.config.workspace_root, true)?
    } else {
        collect_shipcheck(&state.config.workspace_root)?
    };

    let freshness = if state.config.project_memory.enabled {
        Some(project_index_freshness(&state.config)?)
    } else {
        None
    };
    print_shipcheck(&snapshot, freshness.as_ref());

    if export {
        let out_path = explicit_path.unwrap_or_else(|| default_export_path(&state.session_dir));
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(
            &out_path,
            render_shipcheck_markdown(&snapshot, freshness.as_ref()),
        )?;
        println!(
            "  {GREEN}✓{RESET} {DIM}shipcheck saved →{RESET} {}",
            out_path.display()
        );
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HandoffArgs {
    export: bool,
    explicit_path: Option<PathBuf>,
    allow_cloud: bool,
}

fn parse_handoff_args(args: &str) -> Option<HandoffArgs> {
    let mut export = false;
    let mut explicit_path = None;
    let mut allow_cloud = false;

    for part in args.split_whitespace() {
        match part {
            "--cloud" => allow_cloud = true,
            "export" | "save" if !export => export = true,
            other if export && explicit_path.is_none() => {
                explicit_path = Some(PathBuf::from(other));
            }
            _ => return None,
        }
    }

    Some(HandoffArgs {
        export,
        explicit_path,
        allow_cloud,
    })
}

async fn cmd_handoff(args: &str, state: &AppState) -> Result<()> {
    let Some(args) = parse_handoff_args(args) else {
        println!("  {DIM}Usage: /handoff [export|save] [path] [--cloud]{RESET}");
        return Ok(());
    };

    if should_refuse_cloud_handoff(state.backend.name, args.allow_cloud) {
        println!(
            "  {RED}✗{RESET} {DIM}/handoff will not send diff context to OpenRouter unless you pass --cloud.{RESET}"
        );
        return Ok(());
    }

    let snapshot = collect_shipcheck(&state.config.workspace_root)?;
    let freshness = if state.config.project_memory.enabled {
        Some(project_index_freshness(&state.config)?)
    } else {
        None
    };
    let Some(context) = collect_handoff_context(&snapshot)? else {
        println!(
            "  {DIM}Nothing to hand off: working tree is clean and branch is not ahead of upstream.{RESET}"
        );
        return Ok(());
    };

    println!(
        "  {DIM}drafting handoff from {} with {} · {}{RESET}",
        context.basis.label(),
        state.config.backend.as_str(),
        state.model
    );

    let messages = vec![
        ChatMessage::System {
            content: handoff_system_prompt(),
        },
        ChatMessage::User {
            content: render_handoff_prompt(&context, freshness.as_ref()),
        },
    ];
    let req = ChatRequest {
        model: &state.model,
        messages: &messages,
        tools: None,
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: false,
        }),
        max_tokens: Some(900),
    };
    let mut draft = String::new();
    let result = stream_chat(&state.http, &state.backend, &req, None, |chunk| {
        if let Some(choice) = chunk.choices.first() {
            if let Some(content) = &choice.delta.content {
                draft.push_str(content);
            }
        }
    })
    .await;

    let body = match result {
        Ok(_) if !draft.trim().is_empty() => ensure_required_sections(&draft),
        Ok(_) => render_fallback_markdown(
            &context,
            &snapshot,
            freshness.as_ref(),
            Some("empty model response"),
        ),
        Err(e) => render_fallback_markdown(
            &context,
            &snapshot,
            freshness.as_ref(),
            Some(&e.to_string()),
        ),
    };

    println!();
    print!("{body}");

    if args.export {
        let out_path = args
            .explicit_path
            .unwrap_or_else(|| default_handoff_export_path(&state.session_dir));
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&out_path, body)?;
        println!(
            "  {GREEN}✓{RESET} {DIM}handoff saved →{RESET} {}",
            out_path.display()
        );
    }

    Ok(())
}

fn print_shipcheck(
    snapshot: &ShipcheckSnapshot,
    freshness: Option<&crate::project_memory::ProjectIndexFreshness>,
) {
    let status_color = if snapshot.is_clean() { GREEN } else { YELLOW };
    let status = if snapshot.is_clean() {
        "clean"
    } else {
        "dirty"
    };
    println!("  {DIM}shipcheck{RESET}       {status_color}{status}{RESET}");
    println!(
        "  {DIM}branch{RESET}          {CYAN}{}{RESET}",
        snapshot.branch_label()
    );
    if snapshot.branch.behind > 0 {
        println!(
            "  {YELLOW}!{RESET} {DIM}branch is behind upstream by {} commit(s).{RESET}",
            snapshot.branch.behind
        );
    }
    if snapshot.conflict_count() > 0 {
        println!(
            "  {RED}✗{RESET} {DIM}{} conflicted file(s) need attention before release.{RESET}",
            snapshot.conflict_count()
        );
    }
    println!(
        "  {DIM}files{RESET}           staged={} unstaged={} untracked={} conflicts={}",
        snapshot.staged_count(),
        snapshot.unstaged_count(),
        snapshot.untracked_count(),
        snapshot.conflict_count()
    );
    print_shipcheck_file_preview(snapshot);
    print_diff_stat("stagedDiff", &snapshot.staged_diff_stat);
    print_diff_stat("unstagedDiff", &snapshot.unstaged_diff_stat);

    if let Some(test_status) = &snapshot.test_status {
        let test_color = if test_status.failed > 0 || test_status.exit_code != 0 {
            RED
        } else {
            GREEN
        };
        println!(
            "  {DIM}tests{RESET}           framework={CYAN}{}{RESET} total={} passed={} failed={} skipped={} {test_color}exit_code={}{}{RESET}",
            test_status.framework,
            test_status.total,
            test_status.passed,
            test_status.failed,
            test_status.skipped,
            test_status.exit_code,
            if test_status.failed > 0 || test_status.exit_code != 0 { " [FAILED]" } else { " [PASSED]" }
        );
        if let Some(error) = &test_status.error {
            println!("  {RED}✗{RESET} {DIM}test execution error: {error}{RESET}");
        }
    } else {
        println!("  {DIM}tests{RESET}           not run (use --tests flag)");
    }

    print_project_memory_freshness(freshness);
}

fn print_shipcheck_file_preview(snapshot: &ShipcheckSnapshot) {
    if snapshot.files.is_empty() {
        return;
    }
    for file in snapshot.files.iter().take(8) {
        let origin = file
            .original_path
            .as_ref()
            .map(|path| format!(" from {path}"))
            .unwrap_or_default();
        println!(
            "  {DIM}change{RESET}          {}{origin} {DIM}({}){RESET}",
            file.path,
            file_status_label(file)
        );
    }
    if snapshot.files.len() > 8 {
        println!(
            "  {DIM}change{RESET}          …and {} more{RESET}",
            snapshot.files.len() - 8
        );
    }
}

fn print_diff_stat(label: &str, stat: &str) {
    if stat.trim().is_empty() {
        println!("  {DIM}{label}{RESET}      none");
    } else {
        println!("  {DIM}{label}{RESET}");
        for line in stat.lines() {
            println!("    {DIM}{line}{RESET}");
        }
    }
}

fn print_project_memory_freshness(
    freshness: Option<&crate::project_memory::ProjectIndexFreshness>,
) {
    match freshness {
        Some(report) if report.indexed_files > 0 || report.workspace_files > 0 => {
            let color = if report.is_fresh() { GREEN } else { YELLOW };
            println!(
                "  {DIM}projectMemory{RESET}   {color}{} fresh{RESET} · stale={} missing={} deleted={} errors={}",
                report.fresh,
                report.stale,
                report.missing,
                report.deleted,
                report.read_errors
            );
        }
        Some(_) => println!("  {DIM}projectMemory{RESET}   not indexed"),
        None => println!("  {DIM}projectMemory{RESET}   disabled"),
    }
}

fn cmd_sessions(args: &str, state: &AppState) -> Result<()> {
    let args = args.trim();
    if let Some(query) = args.strip_prefix("search ") {
        let hits = search_sessions(&state.session_dir, query)?;
        if hits.is_empty() {
            println!("  {DIM}No sessions matched `{}`.{RESET}", query.trim());
            return Ok(());
        }
        for hit in hits.into_iter().take(20) {
            println!(
                "  {CYAN}{}{RESET} {DIM}{} match(es) · {} · {}{RESET}",
                hit.summary.id,
                hit.matches,
                hit.summary.title.as_deref().unwrap_or("untitled"),
                hit.preview
            );
        }
        return Ok(());
    }
    if let Some(id) = args.strip_prefix("delete ") {
        let mut parts: Vec<&str> = id.split_whitespace().collect();
        let confirmed = parts.iter().any(|part| *part == "--yes" || *part == "yes");
        parts.retain(|part| *part != "--yes" && *part != "yes");
        let id = parts.join(" ");
        if id.is_empty() {
            println!("  {DIM}Usage: /sessions delete <id> --yes{RESET}");
            return Ok(());
        }
        if !confirmed {
            println!("  {YELLOW}!{RESET} {DIM}Confirm with /sessions delete {id} --yes{RESET}");
            return Ok(());
        }
        match delete_session(&state.session_dir, &id)? {
            Some(path) => println!(
                "  {GREEN}✓{RESET} {DIM}deleted session {}{RESET}",
                path.display()
            ),
            None => println!("  {YELLOW}!{RESET} {DIM}session not found: {id}{RESET}"),
        }
        return Ok(());
    }
    if args.starts_with("prune") {
        let confirmed = args
            .split_whitespace()
            .any(|part| part == "--yes" || part == "yes");
        if !confirmed {
            println!("  {YELLOW}!{RESET} {DIM}Confirm with /sessions prune --yes (keeps 20 newest sessions).{RESET}");
            return Ok(());
        }
        let sessions = list_sessions(&state.session_dir)?;
        let mut removed = 0usize;
        for session in sessions.into_iter().skip(20) {
            if delete_session(&state.session_dir, &session.id)?.is_some() {
                removed += 1;
            }
        }
        println!("  {GREEN}✓{RESET} {DIM}pruned {removed} old session(s).{RESET}");
        return Ok(());
    }
    if !args.is_empty() {
        println!("  {DIM}Usage: /sessions [search <query>|delete <id> --yes|prune --yes]{RESET}");
        return Ok(());
    }
    let sessions = list_sessions(&state.session_dir)?;
    if sessions.is_empty() {
        println!("  {DIM}No sessions saved yet.{RESET}");
        return Ok(());
    }
    for session in sessions.into_iter().take(20) {
        println!(
            "  {CYAN}{}{RESET} {DIM}{} messages · {} bytes · {} · {}{RESET}",
            session.id,
            session.messages,
            session.bytes,
            format_system_time(session.modified),
            session.title.as_deref().unwrap_or("untitled")
        );
    }
    Ok(())
}

fn cmd_resume(args: &str, state: &mut AppState) -> Result<()> {
    let id = if args.is_empty() { "latest" } else { args };
    let Some(path) = resolve_session_path(&state.session_dir, id)? else {
        println!("  {RED}✗{RESET} {DIM}Session not found: {id}{RESET}");
        return Ok(());
    };
    let messages = load_messages(&path)?;
    state.messages = messages;
    state.session_path = path.clone();
    println!(
        "  {GREEN}✓{RESET} {DIM}resumed{RESET} {CYAN}{}{RESET} {DIM}({} messages){RESET}",
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session"),
        state.messages.len()
    );
    Ok(())
}

fn messages_to_entries(messages: &[ChatMessage]) -> Vec<SessionEntry> {
    messages
        .iter()
        .cloned()
        .map(|message| SessionEntry {
            timestamp: Utc::now().to_rfc3339(),
            message,
        })
        .collect()
}

fn cmd_export(args: &str, state: &AppState) -> Result<()> {
    let mut parts = args.split_whitespace();
    let target = parts.next().unwrap_or("current");
    let format = parts.next().unwrap_or("markdown");
    let explicit_path = parts.next();
    let (entries, id) = if target == "current" {
        let entries = if state.session_path.exists() {
            load_session(&state.session_path)
                .unwrap_or_else(|_| messages_to_entries(&state.messages))
        } else {
            messages_to_entries(&state.messages)
        };
        let id = state
            .session_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("current")
            .to_string();
        (entries, id)
    } else {
        let Some(path) = resolve_session_path(&state.session_dir, target)? else {
            println!("  {RED}✗{RESET} {DIM}Session not found: {target}{RESET}");
            return Ok(());
        };
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session")
            .to_string();
        (load_session(&path)?, id)
    };
    let ext = if format == "json" { "json" } else { "md" };
    let out_path = explicit_path
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&state.session_dir).join(format!("{id}.{ext}")));
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let body = if format == "json" {
        serde_json::to_string_pretty(&entries)?
    } else {
        render_markdown(&entries)
    };
    fs::write(&out_path, body)?;
    println!(
        "  {GREEN}✓{RESET} {DIM}exported →{RESET} {}",
        out_path.display()
    );
    Ok(())
}

async fn cmd_backend(args: &str, state: &mut AppState) -> Result<()> {
    let chosen: Option<BackendName> = if !args.is_empty() {
        BackendName::parse(args)
    } else {
        println!(
            "  {DIM}Current:{RESET} {CYAN}{}{RESET}",
            state.config.backend.as_str()
        );
        for (i, b) in BackendName::all().iter().enumerate() {
            println!("  {DIM}{}){RESET} {}", i + 1, b.as_str());
        }
        let prompt = format!("  {DIM}Select (1-{}):{RESET} ", BackendName::all().len());
        let pick = plain_read_line(prompt).await?.trim().to_string();
        pick.parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| BackendName::all().get(i).copied())
    };
    let Some(chosen) = chosen else {
        println!("  {DIM}Cancelled.{RESET}");
        return Ok(());
    };
    if matches!(chosen, BackendName::Openrouter) && backend(chosen).api_key.is_empty() {
        println!("  {RED}✗{RESET} {DIM}OPENROUTER_API_KEY not set in environment.{RESET}");
        return Ok(());
    }
    state.config.backend = chosen;
    state.config.model_override = None;
    state.rebuild_client()?;
    state.resolve_model();
    println!(
        "  {GREEN}✓{RESET} {DIM}backend →{RESET} {CYAN}{}{RESET} {DIM}· model →{RESET} {CYAN}{}{RESET}",
        chosen.as_str(),
        state.model
    );
    Ok(())
}

fn profile_names(state: &AppState) -> Vec<String> {
    let mut names: Vec<String> = ProfileName::all()
        .iter()
        .map(|p| p.as_str().to_string())
        .collect();
    for name in state.config.profiles.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

async fn cmd_profile(args: &str, state: &mut AppState) -> Result<()> {
    let names = profile_names(state);
    let chosen: Option<String> = if !args.is_empty() {
        if names.iter().any(|n| n == args) || state.config.profiles.contains_key(args) {
            Some(args.to_string())
        } else {
            None
        }
    } else {
        println!(
            "  {DIM}Current:{RESET} {CYAN}{}{RESET}",
            state.config.profile.as_str()
        );
        for (i, p) in names.iter().enumerate() {
            println!("  {DIM}{}){RESET} {}", i + 1, p);
        }
        let prompt = format!("  {DIM}Select (1-{}):{RESET} ", names.len());
        let pick = plain_read_line(prompt).await?.trim().to_string();
        pick.parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| names.get(i).cloned())
    };
    let Some(chosen) = chosen else {
        println!("  {DIM}Cancelled.{RESET}");
        return Ok(());
    };
    state.config.profile = chosen.clone();
    state.config.model_override = None;
    state.resolve_model();
    println!(
        "  {GREEN}✓{RESET} {DIM}profile →{RESET} {CYAN}{}{RESET} {DIM}· model →{RESET} {CYAN}{}{RESET}",
        chosen,
        state.model
    );
    Ok(())
}

async fn cmd_model(args: &str, state: &mut AppState) -> Result<()> {
    use std::io::Write;
    if !args.is_empty() {
        state.config.model_override = Some(args.to_string());
        state.resolve_model();
        println!(
            "  {GREEN}✓{RESET} {DIM}model →{RESET} {CYAN}{}{RESET}",
            state.model
        );
        return Ok(());
    }
    let mut out = std::io::stdout();
    let _ = write!(
        out,
        "  {DIM}Fetching models from {}…{RESET}",
        state.config.backend.as_str()
    );
    let _ = out.flush();
    let ids = match list_models(&state.http, &state.backend).await {
        Ok(v) => v,
        Err(e) => {
            let _ = write!(out, "\r\x1b[K");
            println!("  {RED}✗{RESET} {DIM}Failed: {e}{RESET}");
            return Ok(());
        }
    };
    let _ = write!(out, "\r\x1b[K");
    let _ = out.flush();
    if ids.is_empty() {
        println!("  {DIM}No models available.{RESET}");
        return Ok(());
    }
    let prompt = format!("  {DIM}Filter (blank for all):{RESET} ");
    let filter = plain_read_line(prompt).await?.trim().to_lowercase();
    let matches: Vec<String> = if filter.is_empty() {
        ids
    } else {
        ids.into_iter()
            .filter(|m| m.to_lowercase().contains(&filter))
            .collect()
    };
    let total = matches.len();
    let shown: Vec<String> = matches.into_iter().take(20).collect();
    if shown.is_empty() {
        println!("  {DIM}No matches.{RESET}");
        return Ok(());
    }
    for (i, m) in shown.iter().enumerate() {
        println!("  {DIM}{:>2}){RESET} {}", i + 1, m);
    }
    if total > shown.len() {
        println!("  {DIM}…and {} more{RESET}", total - shown.len());
    }
    let prompt = format!("  {DIM}Select (1-{}):{RESET} ", shown.len());
    let pick = plain_read_line(prompt).await?.trim().to_string();
    if let Some(idx) = pick.parse::<usize>().ok().and_then(|n| n.checked_sub(1)) {
        if let Some(m) = shown.get(idx) {
            state.config.model_override = Some(m.clone());
            state.resolve_model();
            println!(
                "  {GREEN}✓{RESET} {DIM}model →{RESET} {CYAN}{}{RESET}",
                state.model
            );
            return Ok(());
        }
    }
    println!("  {DIM}Cancelled.{RESET}");
    Ok(())
}

fn cmd_tools(args: &str, state: &mut AppState) {
    if args.is_empty() {
        println!("  {DIM}available{RESET}  {}", ALL_TOOL_NAMES.join(", "));
        println!(
            "  {DIM}enabled{RESET}    {CYAN}{}{RESET}",
            state.config.tools.join(", ")
        );
        println!(
            "  {DIM}mode{RESET}       {CYAN}{}{RESET}",
            state.config.tool_selection.as_str()
        );
        println!(
            "  {DIM}usage{RESET}      /tools auto · /tools fixed · /tools file_read,grep,list_dir"
        );
        return;
    }

    let (mode, list) = if args == "auto" {
        state.config.tool_selection = ToolSelection::Auto;
        state.config.mode = OperatorMode::Custom;
        println!("  {GREEN}✓{RESET} {DIM}tool selection →{RESET} {CYAN}auto{RESET}");
        return;
    } else if args == "fixed" {
        state.config.tool_selection = ToolSelection::Fixed;
        state.config.mode = OperatorMode::Custom;
        println!("  {GREEN}✓{RESET} {DIM}tool selection →{RESET} {CYAN}fixed{RESET}");
        return;
    } else if let Some(rest) = args.strip_prefix("auto ") {
        (ToolSelection::Auto, rest)
    } else if let Some(rest) = args.strip_prefix("fixed ") {
        (ToolSelection::Fixed, rest)
    } else {
        (ToolSelection::Fixed, args)
    };

    let requested: Vec<String> = list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let invalid: Vec<&String> = requested.iter().filter(|n| !is_tool_name(n)).collect();
    if !invalid.is_empty() {
        let names: Vec<&str> = invalid.iter().map(|s| s.as_str()).collect();
        println!(
            "  {RED}✗{RESET} {DIM}unknown tools: {}{RESET}",
            names.join(", ")
        );
        return;
    }
    state.config.tools = requested;
    state.config.tool_selection = mode;
    state.config.mode = OperatorMode::Custom;
    println!(
        "  {GREEN}✓{RESET} {DIM}tools →{RESET} {CYAN}{}{RESET} {DIM}· mode →{RESET} {CYAN}{}{RESET}",
        state.config.tools.join(", "),
        state.config.tool_selection.as_str()
    );
}

async fn cmd_compare(args: &str, state: &AppState) -> Result<()> {
    use std::io::Write;
    let cloud_backend = backend(BackendName::Openrouter);
    if cloud_backend.api_key.is_empty() {
        println!("  {RED}✗{RESET} {DIM}OPENROUTER_API_KEY not set.{RESET}");
        return Ok(());
    }
    let last_user = state.messages.iter().rev().find_map(|m| m.user_text());
    let Some(user_text) = last_user else {
        println!("  {DIM}No user message yet.{RESET}");
        return Ok(());
    };
    let cloud_model = if !args.is_empty() {
        args.to_string()
    } else {
        default_model(
            &cloud_backend,
            &state.config.profile,
            None,
            &state.config.profiles,
        )
    };
    println!("  {YELLOW}⇆{RESET} {BOLD}cloud{RESET} {DIM}{cloud_model}{RESET}");
    println!();

    let messages = vec![
        ChatMessage::System {
            content: state.config.render_system_prompt(),
        },
        ChatMessage::User {
            content: user_text.to_string(),
        },
    ];
    let req = ChatRequest {
        model: &cloud_model,
        messages: &messages,
        tools: None,
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: false,
        }),
        max_tokens: None,
    };
    let mut out = std::io::stdout();
    let result = stream_chat(&state.http, &cloud_backend, &req, None, |chunk| {
        if let Some(c) = chunk.choices.first() {
            if let Some(content) = &c.delta.content {
                let _ = out.write_all(content.as_bytes());
                let _ = out.flush();
            }
        }
    })
    .await;
    println!();
    if let Err(e) = result {
        println!("  {RED}✗{RESET} {DIM}{e}{RESET}");
    }
    Ok(())
}

fn format_system_time(t: SystemTime) -> String {
    let dt: chrono::DateTime<Utc> = t.into();
    dt.format("%Y-%m-%d %H:%M:%SZ").to_string()
}

fn transcript_bytes(messages: &[ChatMessage]) -> usize {
    serde_json::to_vec(messages).map(|v| v.len()).unwrap_or(0)
}

fn cmd_context(args: &str, state: &mut AppState) {
    if !args.is_empty() {
        for part in args.split_whitespace() {
            if let Some(value) = part.strip_prefix("maxMessages=") {
                if let Ok(n) = value.parse::<usize>() {
                    state.config.context.max_messages = Some(n);
                }
            } else if let Some(value) = part.strip_prefix("maxBytes=") {
                if let Ok(n) = value.parse::<usize>() {
                    state.config.context.max_bytes = Some(n);
                }
            } else if let Some(value) = part.strip_prefix("modelTokens=") {
                if let Ok(n) = value.parse::<usize>() {
                    state.config.context.model_context_tokens = Some(n);
                }
            } else if let Some(value) = part.strip_prefix("autoCompact=") {
                state.config.context.auto_compact = Some(matches!(value, "on" | "true" | "1"));
            } else if let Some(value) = part.strip_prefix("compactThreshold=") {
                if let Ok(n) = value.parse::<f64>() {
                    state.config.context.compact_threshold = n.clamp(0.5, 0.99);
                }
            } else if let Some(value) = part.strip_prefix("reserveRatio=") {
                if let Ok(n) = value.parse::<f64>() {
                    state.config.context.reserve_ratio = n.clamp(0.05, 0.5);
                }
            }
        }
    }
    let last_prompt = last_user_prompt(state).unwrap_or_default();
    let active_tool_names = select_tool_names(&state.config, &last_prompt);
    let system_prompt = render_system_prompt_with_memory(
        &state.config,
        &state.backend,
        &active_tool_names,
        &last_prompt,
    );
    let tools = build_tools_for_names(&state.config, &active_tool_names);
    let tool_defs = to_openai_tools(&tools);
    let budget = measure_prompt_budget(&system_prompt, &state.messages, &tool_defs);
    println!("  {DIM}messages{RESET}  {}", state.messages.len());
    println!(
        "  {DIM}mode{RESET}      {}",
        state.config.tool_selection.as_str()
    );
    println!(
        "  {DIM}tools{RESET}     {}",
        if active_tool_names.is_empty() {
            "none".to_string()
        } else {
            active_tool_names.join(", ")
        }
    );
    println!(
        "  {DIM}bytes{RESET}     {}",
        transcript_bytes(&state.messages)
    );
    println!(
        "  {DIM}budget{RESET}    total={} (~{} tokens)",
        format_bytes(budget.effective_total_bytes),
        budget.estimated_tokens
    );
    println!(
        "  {DIM}breakdown{RESET} system={} transcript={} toolSchemas={} toolResults={}",
        format_bytes(budget.system_bytes),
        format_bytes(budget.transcript_bytes),
        format_bytes(budget.tool_schema_bytes),
        format_bytes(budget.tool_result_bytes)
    );
    println!(
        "  {DIM}limits{RESET}    maxMessages={:?} maxBytes={:?} modelTokens={:?}",
        state.config.context.max_messages,
        state.config.context.max_bytes,
        state.config.context.model_context_tokens
    );
    for line in context_status_lines(
        &state.config,
        &state.model,
        state.backend.is_local,
        &budget,
        state.context_guard_notice.as_deref(),
    ) {
        println!("{line}");
    }
}

fn cmd_index(args: &str, state: &AppState) -> Result<()> {
    let arg = args.trim();
    match arg {
        "" => {
            let status = project_memory_status(&state.config)?;
            if status.exists {
                print_project_memory_status(&status);
            } else {
                let index = build_project_index(&state.config)?;
                print_index_built(&index);
            }
        }
        "status" => {
            let status = project_memory_status(&state.config)?;
            print_project_memory_status(&status);
            if status.exists {
                let freshness = project_index_freshness(&state.config)?;
                print_project_index_freshness(&freshness);
            }
        }
        "refresh" | "refresh changed" | "changed" => {
            let index = refresh_changed_project_index(&state.config)?;
            print_index_built(&index);
        }
        "clear" => {
            if clear_project_index(&state.config)? {
                println!("  {GREEN}✓{RESET} {DIM}project memory index cleared.{RESET}");
            } else {
                println!("  {DIM}No project memory index to clear.{RESET}");
            }
        }
        other => println!(
            "  {DIM}Usage: /index [refresh|refresh changed|status|clear] (got {other}){RESET}"
        ),
    }
    Ok(())
}

fn print_index_built(index: &crate::project_memory::ProjectIndex) {
    println!(
        "  {GREEN}✓{RESET} {DIM}indexed {} files under {}{RESET}",
        index.files.len(),
        index.workspace_root
    );
    println!(
        "  {DIM}skipped{RESET} ignored={} oversized={} binary={} outside={} errors={}",
        index.skipped.ignored,
        index.skipped.oversized,
        index.skipped.binary,
        index.skipped.outside_workspace,
        index.skipped.read_errors
    );
}

fn print_project_memory_status(status: &crate::project_memory::ProjectMemoryStatus) {
    if status.exists {
        println!(
            "  {GREEN}✓{RESET} {DIM}project memory index:{RESET} {}",
            status.path.display()
        );
        println!(
            "  {DIM}files{RESET} {}  {DIM}bytes{RESET} {}  {DIM}generated{RESET} {}",
            status.files,
            status.bytes,
            status.generated_at.as_deref().unwrap_or("unknown")
        );
    } else {
        println!(
            "  {YELLOW}!{RESET} {DIM}project memory index missing:{RESET} {}",
            status.path.display()
        );
        println!("  {DIM}Run /index to build it.{RESET}");
    }
}

fn print_project_index_freshness(freshness: &crate::project_memory::ProjectIndexFreshness) {
    let marker = if freshness.is_fresh() { GREEN } else { YELLOW };
    let label = if freshness.is_fresh() {
        "fresh"
    } else {
        "stale"
    };
    println!(
        "  {marker}{label}{RESET} {DIM}workspaceFiles={} indexed={} fresh={} stale={} missing={} deleted={} errors={}{RESET}",
        freshness.workspace_files,
        freshness.indexed_files,
        freshness.fresh,
        freshness.stale,
        freshness.missing,
        freshness.deleted,
        freshness.read_errors
    );
}

fn cmd_map(args: &str, state: &AppState) -> Result<()> {
    let Some(index) = load_project_index(&state.config)? else {
        println!("  {YELLOW}!{RESET} {DIM}project memory index missing. Run /index first.{RESET}");
        return Ok(());
    };
    let notes = load_project_notes(&state.config)?;
    let query = if args.trim().is_empty() {
        None
    } else {
        Some(args.trim())
    };
    let map = render_repo_map(&state.config, &index, &notes, query);
    print!("{}", map.content);
    if map.truncated {
        println!("  {DIM}map truncated at {} bytes{RESET}", map.bytes);
    }
    Ok(())
}

fn cmd_memory(args: &str, state: &mut AppState) {
    match args.trim() {
        "" | "status" => {
            println!(
                "  {DIM}projectMemory{RESET} enabled={} autoInject={} autoIndex={} allowCloudContext={}",
                state.config.project_memory.enabled,
                state.config.project_memory.auto_inject,
                state.config.project_memory.auto_index,
                state.config.project_memory.allow_cloud_context
            );
            if let Ok(status) = project_memory_status(&state.config) {
                print_project_memory_status(&status);
            }
        }
        "on" => {
            state.config.project_memory.enabled = true;
            println!("  {GREEN}✓{RESET} {DIM}project memory enabled for this session.{RESET}");
        }
        "off" => {
            state.config.project_memory.enabled = false;
            println!("  {GREEN}✓{RESET} {DIM}project memory disabled for this session.{RESET}");
        }
        other => println!("  {DIM}Usage: /memory [on|off|status] (got {other}){RESET}"),
    }
}

fn cmd_remember(args: &str, state: &AppState) -> Result<()> {
    if args.trim().is_empty() {
        println!("  {DIM}Usage: /remember <project note>{RESET}");
        return Ok(());
    }
    let note = append_project_note(&state.config, args)?;
    println!(
        "  {GREEN}✓{RESET} {DIM}remembered{RESET} {CYAN}{}{RESET}",
        note.id
    );
    Ok(())
}

fn cmd_forget(args: &str, state: &AppState) -> Result<()> {
    let id = args.trim();
    if id.is_empty() {
        println!("  {DIM}Usage: /forget <id|all>{RESET}");
        return Ok(());
    }
    let removed = forget_project_note(&state.config, id)?;
    if id == "all" {
        println!("  {GREEN}✓{RESET} {DIM}forgot all project notes.{RESET}");
    } else if removed == 0 {
        println!("  {YELLOW}!{RESET} {DIM}project note not found: {id}{RESET}");
    } else {
        println!("  {GREEN}✓{RESET} {DIM}forgot project note {id}.{RESET}");
    }
    Ok(())
}

async fn cmd_compact(args: &str, state: &mut AppState) -> Result<()> {
    let keep = if args.is_empty() {
        None
    } else {
        Some(args.parse::<usize>().unwrap_or(12).clamp(4, 80))
    };
    let last_prompt = last_user_prompt(state).unwrap_or_default();
    let active_tool_names = select_tool_names(&state.config, &last_prompt);
    let system_prompt = render_system_prompt_with_memory(
        &state.config,
        &state.backend,
        &active_tool_names,
        &last_prompt,
    );
    let tools = build_tools_for_names(&state.config, &active_tool_names);
    let tool_defs = to_openai_tools(&tools);

    if state.messages.len() <= keep.unwrap_or(state.config.context.max_messages.unwrap_or(12)) + 1 {
        println!("  {DIM}Nothing to compact yet.{RESET}");
        return Ok(());
    }

    println!("  {DIM}Compacting older messages…{RESET}");
    let result = compact_session(
        &mut state.messages,
        &state.session_dir,
        &mut state.session_path,
        &system_prompt,
        &tool_defs,
        &state.config,
        &state.model,
        state.backend.is_local,
        &state.http,
        &state.backend,
        keep,
        true,
    )
    .await?;

    if !result.compacted {
        println!("  {DIM}Nothing to compact yet.{RESET}");
        return Ok(());
    }

    let method = match result.method {
        CompactMethod::LlmSummary => "summarized",
        CompactMethod::DeterministicTrim => "trimmed",
        CompactMethod::None => "compacted",
    };
    state.context_guard_notice = Some(format!(
        "Compacted {} messages → {} ({method}), budget {:.0}% → {:.0}%",
        result.before_messages,
        result.after_messages,
        result.before_ratio * 100.0,
        result.after_ratio * 100.0
    ));
    println!(
        "  {GREEN}✓{RESET} {DIM}{}{RESET}",
        state.context_guard_notice.as_deref().unwrap_or("")
    );
    println!(
        "  {DIM}session → {}{RESET}",
        state.session_path.display()
    );
    Ok(())
}

async fn cmd_doctor(args: &str, state: &AppState) -> Result<()> {
    let deep = args
        .split_whitespace()
        .any(|arg| arg == "--deep" || arg == "deep");
    let all = args.split_whitespace().any(|arg| arg == "all");
    if deep {
        return cmd_doctor_deep(state, all).await;
    }

    println!("  {BOLD}Small Harness doctor{RESET}");
    println!(
        "  {DIM}backend{RESET} {} · {}",
        state.config.backend.as_str(),
        state.backend.base_url
    );
    match list_models(&state.http, &state.backend).await {
        Ok(models) => println!(
            "  {GREEN}✓{RESET} {DIM}models reachable ({}){RESET}",
            models.len()
        ),
        Err(e) => println!("  {RED}✗{RESET} {DIM}models unreachable: {e}{RESET}"),
    }
    let rg = tokio::process::Command::new("rg")
        .arg("--version")
        .output()
        .await;
    match rg {
        Ok(o) if o.status.success() => println!("  {GREEN}✓{RESET} {DIM}ripgrep available{RESET}"),
        _ => println!("  {YELLOW}!{RESET} {DIM}ripgrep unavailable; grep tool will fail{RESET}"),
    }
    fs::create_dir_all(&state.session_dir)?;
    let probe = Path::new(&state.session_dir).join(".doctor-write-test");
    match fs::write(&probe, "ok").and_then(|_| fs::remove_file(&probe)) {
        Ok(_) => println!("  {GREEN}✓{RESET} {DIM}session dir writable{RESET}"),
        Err(e) => println!("  {RED}✗{RESET} {DIM}session dir not writable: {e}{RESET}"),
    }
    if matches!(state.config.backend, BackendName::Openrouter) && state.backend.api_key.is_empty() {
        println!("  {RED}✗{RESET} {DIM}OPENROUTER_API_KEY missing{RESET}");
    }
    println!(
        "  {DIM}workspace{RESET} {} ({})",
        state.config.workspace_root,
        state.config.outside_workspace.as_str()
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCapabilityReport {
    generated_at: String,
    active_backend: String,
    rows: Vec<DoctorCapabilityRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCapabilityRow {
    backend: String,
    base_url: String,
    model: String,
    models: ProbeStatus,
    streaming: ProbeStatus,
    usage_chunks: ProbeStatus,
    tool_calls: ProbeStatus,
    inline_tool_json: ProbeStatus,
    warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeStatus {
    ok: bool,
    detail: String,
}

impl ProbeStatus {
    fn ok(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            detail: detail.into(),
        }
    }

    fn fail(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: detail.into(),
        }
    }
}

async fn with_probe_timeout<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    match tokio::time::timeout(Duration::from_secs(8), future).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("timed out after 8s")),
    }
}

fn doctor_backend_model(
    backend_desc: &BackendDescriptor,
    listed_models: &[String],
    state: &AppState,
) -> String {
    if backend_desc.name == state.backend.name {
        return state.model.clone();
    }
    listed_models.first().cloned().unwrap_or_else(|| {
        default_model(
            backend_desc,
            &state.config.profile,
            None,
            &state.config.profiles,
        )
    })
}

async fn probe_streaming(
    state: &AppState,
    backend_desc: &BackendDescriptor,
    model: &str,
) -> (ProbeStatus, ProbeStatus) {
    let messages = vec![ChatMessage::User {
        content: "Reply with exactly: ok".into(),
    }];
    let req = ChatRequest {
        model,
        messages: &messages,
        tools: None,
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        max_tokens: Some(8),
    };
    let mut chunks = 0usize;
    let mut content = String::new();
    let mut saw_usage = false;
    let result = with_probe_timeout(stream_chat(
        &state.http,
        backend_desc,
        &req,
        None,
        |chunk| {
            chunks += 1;
            if chunk.usage.is_some() {
                saw_usage = true;
            }
            if let Some(choice) = chunk.choices.first() {
                if let Some(delta) = &choice.delta.content {
                    content.push_str(delta);
                }
            }
        },
    ))
    .await;
    match result {
        Ok(_) => (
            ProbeStatus::ok(format!("{chunks} chunks, {:?}", content.trim())),
            if saw_usage {
                ProbeStatus::ok("usage chunk observed")
            } else {
                ProbeStatus::fail("no usage chunk observed")
            },
        ),
        Err(e) => (
            ProbeStatus::fail(e.to_string()),
            ProbeStatus::fail("streaming probe failed"),
        ),
    }
}

fn doctor_noop_tool() -> Vec<ToolDef> {
    vec![ToolDef {
        kind: "function",
        function: ToolDefFunction {
            name: "doctor_noop".into(),
            description: "Harmless diagnostic tool. Call this when asked.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" }
                },
                "required": ["ok"]
            }),
        },
    }]
}

fn looks_like_inline_tool_json(content: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content.trim()) else {
        return false;
    };
    parsed
        .get("name")
        .and_then(|v| v.as_str())
        .map(|name| name == "doctor_noop")
        .unwrap_or(false)
}

async fn probe_tool_calls(
    state: &AppState,
    backend_desc: &BackendDescriptor,
    model: &str,
) -> (ProbeStatus, ProbeStatus) {
    let messages = vec![ChatMessage::User {
        content: "Call the doctor_noop tool with ok=true. Do not answer in prose.".into(),
    }];
    let tools = doctor_noop_tool();
    let req = ChatRequest {
        model,
        messages: &messages,
        tools: Some(&tools),
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: false,
        }),
        max_tokens: Some(128),
    };
    let mut content = String::new();
    let mut tool_name = String::new();
    let mut tool_args = String::new();
    let result = with_probe_timeout(stream_chat(
        &state.http,
        backend_desc,
        &req,
        None,
        |chunk| {
            if let Some(choice) = chunk.choices.first() {
                if let Some(delta) = &choice.delta.content {
                    content.push_str(delta);
                }
                if let Some(tool_calls) = &choice.delta.tool_calls {
                    for call in tool_calls {
                        if let Some(function) = &call.function {
                            if let Some(name) = &function.name {
                                tool_name.push_str(name);
                            }
                            if let Some(args) = &function.arguments {
                                tool_args.push_str(args);
                            }
                        }
                    }
                }
            }
        },
    ))
    .await;
    match result {
        Ok(_) => {
            let native_ok = tool_name == "doctor_noop";
            let inline_ok = looks_like_inline_tool_json(&content);
            let native_detail = if native_ok {
                let args = if tool_args.is_empty() {
                    "{}"
                } else {
                    tool_args.as_str()
                };
                format!("native tool_calls: {args}")
            } else if inline_ok {
                "no native tool_calls; model emitted inline JSON".into()
            } else if content.trim().is_empty() {
                "no tool call or content observed".into()
            } else {
                format!("no native tool_calls; content {:?}", content.trim())
            };
            (
                ProbeStatus {
                    ok: native_ok,
                    detail: native_detail,
                },
                ProbeStatus {
                    ok: inline_ok,
                    detail: if inline_ok {
                        "inline JSON fallback likely usable".into()
                    } else {
                        "inline JSON fallback not observed".into()
                    },
                },
            )
        }
        Err(e) => (
            ProbeStatus::fail(e.to_string()),
            ProbeStatus::fail("tool probe failed"),
        ),
    }
}

fn doctor_warning(row: &DoctorCapabilityRow) -> Option<String> {
    if row.backend == "llamacpp" && row.streaming.ok && !row.tool_calls.ok {
        return Some(
            "llama.cpp is reachable but native tool_calls were not observed; start llama-server with --jinja for native tool calls".into(),
        );
    }
    if row.streaming.ok && !row.usage_chunks.ok {
        return Some("streaming works, but usage chunks are not reported by this backend".into());
    }
    None
}

fn status_to_cache(status: &ProbeStatus) -> CapabilityStatus {
    CapabilityStatus {
        ok: status.ok,
        detail: status.detail.clone(),
    }
}

fn row_to_capability_record(row: &DoctorCapabilityRow, generated_at: &str) -> CapabilityRecord {
    CapabilityRecord {
        generated_at: generated_at.into(),
        backend: row.backend.clone(),
        base_url: row.base_url.clone(),
        model: row.model.clone(),
        models: status_to_cache(&row.models),
        streaming: status_to_cache(&row.streaming),
        usage_chunks: status_to_cache(&row.usage_chunks),
        tool_calls: status_to_cache(&row.tool_calls),
        inline_tool_json: status_to_cache(&row.inline_tool_json),
        warning: row.warning.clone(),
        benchmark: None,
    }
}

async fn probe_backend_capabilities(
    state: &AppState,
    backend_desc: BackendDescriptor,
) -> DoctorCapabilityRow {
    let mut listed_models = Vec::new();
    let models = match validate(&backend_desc) {
        Ok(()) => match with_probe_timeout(list_models(&state.http, &backend_desc)).await {
            Ok(models) => {
                listed_models = models;
                ProbeStatus::ok(format!("{} model(s)", listed_models.len()))
            }
            Err(e) => ProbeStatus::fail(e.to_string()),
        },
        Err(e) => ProbeStatus::fail(e.to_string()),
    };
    let model = doctor_backend_model(&backend_desc, &listed_models, state);
    let (streaming, usage_chunks, tool_calls, inline_tool_json) = if models.ok {
        let (streaming, usage_chunks) = probe_streaming(state, &backend_desc, &model).await;
        let (tool_calls, inline_tool_json) = if streaming.ok {
            probe_tool_calls(state, &backend_desc, &model).await
        } else {
            (
                ProbeStatus::fail("skipped because streaming failed"),
                ProbeStatus::fail("skipped because streaming failed"),
            )
        };
        (streaming, usage_chunks, tool_calls, inline_tool_json)
    } else {
        (
            ProbeStatus::fail("skipped because /models failed"),
            ProbeStatus::fail("skipped because /models failed"),
            ProbeStatus::fail("skipped because /models failed"),
            ProbeStatus::fail("skipped because /models failed"),
        )
    };
    let mut row = DoctorCapabilityRow {
        backend: backend_desc.name.as_str().into(),
        base_url: backend_desc.base_url,
        model,
        models,
        streaming,
        usage_chunks,
        tool_calls,
        inline_tool_json,
        warning: None,
    };
    row.warning = doctor_warning(&row);
    row
}

fn mark(status: &ProbeStatus) -> &'static str {
    if status.ok {
        "yes"
    } else {
        "no"
    }
}

fn print_capability_table(rows: &[DoctorCapabilityRow]) {
    println!();
    println!(
        "  {BOLD}{:<11}{RESET} {:<7} {:<9} {:<6} {:<10} warning",
        "backend", "models", "stream", "usage", "toolcalls"
    );
    for row in rows {
        println!(
            "  {CYAN}{:<11}{RESET} {:<7} {:<9} {:<6} {:<10} {}",
            row.backend,
            mark(&row.models),
            mark(&row.streaming),
            mark(&row.usage_chunks),
            mark(&row.tool_calls),
            row.warning.as_deref().unwrap_or("")
        );
    }
    println!();
    for row in rows {
        println!(
            "  {BOLD}{}{RESET} {DIM}{} · {}{RESET}",
            row.backend, row.base_url, row.model
        );
        println!("    {DIM}models:{RESET} {}", row.models.detail);
        println!("    {DIM}streaming:{RESET} {}", row.streaming.detail);
        println!("    {DIM}usage:{RESET} {}", row.usage_chunks.detail);
        println!("    {DIM}tool_calls:{RESET} {}", row.tool_calls.detail);
        println!(
            "    {DIM}inline_json:{RESET} {}",
            row.inline_tool_json.detail
        );
    }
}

fn render_doctor_markdown(report: &DoctorCapabilityReport) -> String {
    let mut out = format!(
        "# Small Harness Doctor Report\n\nGenerated: `{}`\n\nActive backend: `{}`\n\n",
        report.generated_at, report.active_backend
    );
    out.push_str("| Backend | Models | Streaming | Usage | Tool Calls | Warning |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for row in &report.rows {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            row.backend,
            mark(&row.models),
            mark(&row.streaming),
            mark(&row.usage_chunks),
            mark(&row.tool_calls),
            row.warning.as_deref().unwrap_or("")
        ));
    }
    out.push('\n');
    for row in &report.rows {
        out.push_str(&format!("## `{}`\n\n", row.backend));
        out.push_str(&format!("- Base URL: `{}`\n", row.base_url));
        out.push_str(&format!("- Model: `{}`\n", row.model));
        out.push_str(&format!("- Models: {}\n", row.models.detail));
        out.push_str(&format!("- Streaming: {}\n", row.streaming.detail));
        out.push_str(&format!("- Usage chunks: {}\n", row.usage_chunks.detail));
        out.push_str(&format!("- Tool calls: {}\n", row.tool_calls.detail));
        out.push_str(&format!(
            "- Inline JSON fallback: {}\n\n",
            row.inline_tool_json.detail
        ));
    }
    out
}

fn save_doctor_report(
    state: &AppState,
    report: &DoctorCapabilityReport,
) -> Result<(PathBuf, PathBuf)> {
    let dir = Path::new(&state.session_dir).join("doctor");
    fs::create_dir_all(&dir)?;
    let id = Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ").to_string();
    let json_path = dir.join(format!("{id}.json"));
    let md_path = dir.join(format!("{id}.md"));
    fs::write(&json_path, serde_json::to_string_pretty(report)?)?;
    fs::write(&md_path, render_doctor_markdown(report))?;
    Ok((json_path, md_path))
}

async fn cmd_doctor_deep(state: &AppState, all: bool) -> Result<()> {
    println!("  {BOLD}Small Harness doctor --deep{RESET}");
    println!(
        "  {DIM}probing OpenAI-compatible model, stream, usage, and tool-call behavior{RESET}"
    );
    let backends: Vec<BackendName> = if all {
        BackendName::all().to_vec()
    } else {
        vec![state.config.backend]
    };
    let mut rows = Vec::new();
    for name in backends {
        let backend_desc = if name == state.backend.name {
            state.backend.clone()
        } else {
            backend(name)
        };
        println!(
            "  {DIM}probing {} at {}…{RESET}",
            backend_desc.name.as_str(),
            backend_desc.base_url
        );
        rows.push(probe_backend_capabilities(state, backend_desc).await);
    }
    print_capability_table(&rows);
    let report = DoctorCapabilityReport {
        generated_at: Utc::now().to_rfc3339(),
        active_backend: state.config.backend.as_str().into(),
        rows,
    };
    let mut cached = 0usize;
    for row in &report.rows {
        let record = row_to_capability_record(row, &report.generated_at);
        capabilities::save_record(&state.session_dir, record)?;
        cached += 1;
    }
    let (json_path, md_path) = save_doctor_report(state, &report)?;
    println!(
        "  {GREEN}✓{RESET} {DIM}doctor report saved → {} and {}{RESET}",
        json_path.display(),
        md_path.display()
    );
    println!(
        "  {GREEN}✓{RESET} {DIM}cached {} capability record(s) under {}/capabilities{RESET}",
        cached, state.session_dir
    );
    Ok(())
}

async fn cmd_bench(args: &str, state: &AppState) -> Result<()> {
    let model = if args.is_empty() { &state.model } else { args };
    let messages = vec![ChatMessage::User {
        content: "Reply with one short sentence for a latency benchmark.".into(),
    }];
    println!("  {DIM}warming {model}…{RESET}");
    let warm_ms = warmup(&state.http, &state.backend, model, "benchmark", &[])
        .await
        .ok();
    let req = ChatRequest {
        model,
        messages: &messages,
        tools: None,
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: false,
        }),
        max_tokens: None,
    };
    let start = Instant::now();
    let mut first = None;
    let mut chars = 0usize;
    stream_chat(&state.http, &state.backend, &req, None, |chunk| {
        if let Some(choice) = chunk.choices.first() {
            if let Some(content) = &choice.delta.content {
                if first.is_none() {
                    first = Some(start.elapsed());
                }
                chars += content.chars().count();
            }
        }
    })
    .await?;
    let total = start.elapsed();
    let stats = BenchmarkStats::new(
        warm_ms,
        first.map(|d| d.as_millis()),
        total.as_millis(),
        chars,
    );
    let cps = if total.as_secs_f64() > 0.0 {
        chars as f64 / total.as_secs_f64()
    } else {
        0.0
    };
    let cache_path =
        capabilities::save_benchmark(&state.session_dir, &state.backend, model, stats)?;
    println!(
        "  {GREEN}✓{RESET} {DIM}warmup={:?} firstToken={:.2}s total={:.2}s charsPerSec={:.1}{RESET}",
        warm_ms.map(|ms| format!("{:.2}s", ms as f64 / 1000.0)),
        first.map(|d| d.as_secs_f64()).unwrap_or(0.0),
        total.as_secs_f64(),
        cps
    );
    println!(
        "  {GREEN}✓{RESET} {DIM}benchmark cached → {}{RESET}",
        cache_path.display()
    );
    Ok(())
}

async fn refresh_capability_cache(state: &AppState, all: bool) -> Result<Vec<CapabilityRecord>> {
    let backends: Vec<BackendName> = if all {
        BackendName::all().to_vec()
    } else {
        vec![state.config.backend]
    };
    let generated_at = Utc::now().to_rfc3339();
    let mut records = Vec::new();
    for name in backends {
        let backend_desc = if name == state.backend.name {
            state.backend.clone()
        } else {
            backend(name)
        };
        println!(
            "  {DIM}probing {} at {}…{RESET}",
            backend_desc.name.as_str(),
            backend_desc.base_url
        );
        let row = probe_backend_capabilities(state, backend_desc).await;
        let record = row_to_capability_record(&row, &generated_at);
        capabilities::save_record(&state.session_dir, record.clone())?;
        records.push(record);
    }
    Ok(records)
}

fn short_model(model: &str, width: usize) -> String {
    if model.chars().count() <= width {
        return model.into();
    }
    let mut out: String = model.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn bench_label(record: &CapabilityRecord) -> String {
    record
        .benchmark
        .as_ref()
        .map(|bench| {
            format!(
                "{:.1} tok/s · {:.2}s first",
                bench.estimated_tokens_per_sec,
                bench.first_token_ms.unwrap_or(0) as f64 / 1000.0
            )
        })
        .unwrap_or_else(|| "not benched".into())
}

fn print_cached_capabilities(records: &[CapabilityRecord]) {
    println!();
    println!(
        "  {BOLD}{:<11}{RESET} {:<28} {:>5} {:<11} {:<10} benchmark",
        "backend", "model", "score", "tools", "usage"
    );
    for record in sorted_records(records) {
        println!(
            "  {CYAN}{:<11}{RESET} {:<28} {:>5} {:<11} {:<10} {}",
            record.backend,
            short_model(&record.model, 28),
            record_score(&record),
            record.tool_path(),
            mark_cache(&record.usage_chunks),
            bench_label(&record)
        );
        if let Some(warning) = &record.warning {
            println!("    {YELLOW}!{RESET} {DIM}{warning}{RESET}");
        }
    }
}

fn mark_cache(status: &CapabilityStatus) -> &'static str {
    if status.ok {
        "yes"
    } else {
        "no"
    }
}

async fn cmd_capabilities(args: &str, state: &AppState) -> Result<()> {
    let refresh = args
        .split_whitespace()
        .any(|arg| arg == "refresh" || arg == "--refresh");
    let all = args
        .split_whitespace()
        .any(|arg| arg == "all" || arg == "--all");
    if refresh {
        let records = refresh_capability_cache(state, all).await?;
        println!(
            "  {GREEN}✓{RESET} {DIM}cached {} refreshed capability record(s).{RESET}",
            records.len()
        );
    }

    let records = capabilities::load_records(&state.session_dir)?;
    if records.is_empty() {
        println!(
            "  {DIM}No cached capabilities yet. Run /capabilities refresh or /doctor --deep all.{RESET}"
        );
        return Ok(());
    }

    print_cached_capabilities(&records);
    println!(
        "  {DIM}cache{RESET} {}",
        capabilities::cache_dir(&state.session_dir).display()
    );
    Ok(())
}

async fn cmd_autotune(args: &str, state: &mut AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let apply = parts.iter().any(|arg| *arg == "apply" || *arg == "--apply");
    let include_cloud = parts.iter().any(|arg| *arg == "cloud" || *arg == "--cloud");
    let refresh = parts
        .iter()
        .any(|arg| *arg == "refresh" || *arg == "--refresh");
    let all = parts.iter().any(|arg| *arg == "all" || *arg == "--all");

    let mut records = capabilities::load_records(&state.session_dir)?;
    if refresh || records.is_empty() {
        let refreshed = refresh_capability_cache(state, all).await?;
        if !refreshed.is_empty() {
            records = capabilities::load_records(&state.session_dir)?;
        }
    }

    let Some(recommendation) = best_record(&records, include_cloud) else {
        if records.iter().any(CapabilityRecord::is_cloud) && !include_cloud {
            println!(
                "  {DIM}Only cloud-capable cached records were found. Re-run with /autotune --cloud to include them.{RESET}"
            );
        } else {
            println!(
                "  {DIM}No usable cached model yet. Run /autotune refresh all after starting your local backends.{RESET}"
            );
        }
        return Ok(());
    };

    let tool_selection = recommended_tool_selection(&recommendation);
    println!("  {BOLD}Autotune recommendation{RESET}");
    println!(
        "  {DIM}backend{RESET} {} · {DIM}model{RESET} {}",
        recommendation.backend, recommendation.model
    );
    println!(
        "  {DIM}score{RESET} {} · {DIM}tools{RESET} {} · {DIM}toolSelection{RESET} {}",
        record_score(&recommendation),
        recommendation.tool_path(),
        tool_selection.as_str()
    );
    println!(
        "  {DIM}bench{RESET} {} · {DIM}warmup{RESET} {}",
        bench_label(&recommendation),
        if warmup_recommended(&recommendation) {
            "recommended"
        } else {
            "optional"
        }
    );
    if let Some(warning) = &recommendation.warning {
        println!("  {YELLOW}!{RESET} {DIM}{warning}{RESET}");
    }

    if !apply {
        println!("  {DIM}Run /autotune apply to switch this session to the recommendation.{RESET}");
        return Ok(());
    }

    let Some(backend_name) = recommendation.backend_name() else {
        println!(
            "  {RED}✗{RESET} {DIM}Cannot apply unknown backend: {}{RESET}",
            recommendation.backend
        );
        return Ok(());
    };
    let env_backend = backend(backend_name);
    if env_backend.base_url != recommendation.base_url {
        println!(
            "  {YELLOW}!{RESET} {DIM}cached URL was {}, current env resolves to {}{RESET}",
            recommendation.base_url, env_backend.base_url
        );
    }
    state.config.backend = backend_name;
    state.config.model_override = Some(recommendation.model.clone());
    state.config.tool_selection = tool_selection;
    state.rebuild_client()?;
    state.resolve_model();
    println!(
        "  {GREEN}✓{RESET} {DIM}active session tuned → {} · {} · tools={}{RESET}",
        state.config.backend.as_str(),
        state.model,
        state.config.tool_selection.as_str()
    );
    Ok(())
}

fn recommend_backend_names(state: &AppState, all: bool, include_cloud: bool) -> Vec<BackendName> {
    if all {
        BackendName::all()
            .iter()
            .copied()
            .filter(|name| include_cloud || *name != BackendName::Openrouter)
            .collect()
    } else {
        vec![state.config.backend]
    }
}

async fn refresh_recommendation_capabilities(
    state: &AppState,
    all: bool,
    include_cloud: bool,
) -> Result<usize> {
    let generated_at = Utc::now().to_rfc3339();
    let mut cached = 0usize;
    for name in recommend_backend_names(state, all, include_cloud) {
        let backend_desc = if name == state.backend.name {
            state.backend.clone()
        } else {
            backend(name)
        };
        println!(
            "  {DIM}probing {} at {}…{RESET}",
            backend_desc.name.as_str(),
            backend_desc.base_url
        );
        let row = probe_backend_capabilities(state, backend_desc).await;
        let record = row_to_capability_record(&row, &generated_at);
        capabilities::save_record(&state.session_dir, record)?;
        cached += 1;
    }
    Ok(cached)
}

async fn collect_recommendation_candidates(
    state: &AppState,
    spec: &HardwareSpec,
    all: bool,
    include_cloud: bool,
) -> Result<Vec<ModelCandidate>> {
    let mut candidates = Vec::new();
    let profile = spec.recommended_profile();
    for name in recommend_backend_names(state, all, include_cloud) {
        let backend_desc = if name == state.backend.name {
            state.backend.clone()
        } else {
            backend(name)
        };
        let default = default_model(&backend_desc, profile, None, &state.config.profiles);
        let mut default_candidate =
            ModelCandidate::new(backend_desc.name, backend_desc.base_url.clone(), default);
        default_candidate.is_default = true;
        candidates.push(default_candidate);

        if backend_desc.name == state.backend.name {
            let mut current = ModelCandidate::new(
                backend_desc.name,
                backend_desc.base_url.clone(),
                &state.model,
            );
            current.is_current = true;
            candidates.push(current);
        }

        if validate(&backend_desc).is_err() {
            continue;
        }
        let models = match with_probe_timeout(list_models(&state.http, &backend_desc)).await {
            Ok(models) => models,
            Err(_) => continue,
        };
        for model in models {
            let mut candidate =
                ModelCandidate::new(backend_desc.name, backend_desc.base_url.clone(), model);
            candidate.installed = true;
            candidates.push(candidate);
        }
    }

    for record in capabilities::load_records(&state.session_dir)? {
        let Some(backend_name) = record.backend_name() else {
            continue;
        };
        if !include_cloud && backend_name == BackendName::Openrouter {
            continue;
        }
        let mut candidate =
            ModelCandidate::new(backend_name, record.base_url.clone(), record.model.clone());
        candidate.capability = Some(record);
        candidates.push(candidate);
    }

    Ok(candidates)
}

fn hardware_summary(spec: &HardwareSpec) -> String {
    let chip = spec.chip_name.as_deref().unwrap_or("unknown chip");
    let machine = spec.machine_name.as_deref().unwrap_or("unknown machine");
    format!(
        "{} {} · {} · {} · {} · profile {}",
        spec.os,
        spec.arch,
        machine,
        chip,
        spec.memory_label(),
        spec.recommended_profile()
    )
}

fn model_size_label(rec: &ModelRecommendation) -> String {
    let size = rec
        .metadata
        .parameters_b
        .map(|params| format!("{params:.0}B"))
        .unwrap_or_else(|| "unknown size".into());
    let quant = rec
        .metadata
        .quant_bits
        .map(|bits| format!("q{bits}"))
        .unwrap_or_else(|| "quant unknown".into());
    let memory = rec
        .metadata
        .estimated_memory_gb
        .map(|gb| format!("~{gb:.1} GB"))
        .unwrap_or_else(|| "memory unknown".into());
    format!("{size} · {quant} · {memory}")
}

fn backend_model_hint(backend_name: BackendName, model: &str) -> String {
    match backend_name {
        BackendName::Ollama => format!("install with `ollama pull {model}`"),
        BackendName::LmStudio => {
            "load a matching model in LM Studio, then start the Local Server".into()
        }
        BackendName::Mlx => format!("start MLX with `mlx_lm.server --model {model}`"),
        BackendName::LlamaCpp => {
            "start llama.cpp with `llama-server -m /path/to/model.gguf --host 127.0.0.1 --port 8080 --jinja`".into()
        }
        BackendName::Openrouter => "set OPENROUTER_API_KEY before using OpenRouter".into(),
    }
}

fn print_recommendations(spec: &HardwareSpec, recommendations: &[ModelRecommendation]) {
    println!("  {BOLD}Hardware-aware recommendation{RESET}");
    println!("  {DIM}hardware{RESET} {}", hardware_summary(spec));
    println!(
        "  {DIM}tier{RESET} {} · {}",
        spec.tier().as_str(),
        spec.tier().guidance()
    );
    for (idx, rec) in recommendations.iter().take(3).enumerate() {
        println!();
        println!(
            "  {CYAN}{}){RESET} {BOLD}{}{RESET} {DIM}· {}{RESET}",
            idx + 1,
            rec.model,
            rec.backend.as_str()
        );
        println!(
            "     {DIM}score{RESET} {} · {DIM}confidence{RESET} {} · {DIM}fit{RESET} {} · {DIM}installed{RESET} {}",
            rec.score,
            rec.confidence.as_str(),
            rec.memory_fit.as_str(),
            rec.installed
        );
        println!(
            "     {DIM}size{RESET} {} · {DIM}tools{RESET} {} · {DIM}bench{RESET} {}",
            model_size_label(rec),
            rec.tool_path,
            rec.benchmark_label.as_deref().unwrap_or("not benched")
        );
        let why = rec
            .rationale
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        if !why.is_empty() {
            println!("     {DIM}why{RESET} {why}");
        }
    }
}

async fn cmd_recommend(args: &str, state: &mut AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let apply = parts.iter().any(|arg| *arg == "apply" || *arg == "--apply");
    let include_cloud = parts.iter().any(|arg| *arg == "cloud" || *arg == "--cloud");
    let refresh = parts
        .iter()
        .any(|arg| *arg == "refresh" || *arg == "--refresh");
    let all = parts.iter().any(|arg| *arg == "all" || *arg == "--all");

    let spec = detect_hardware_spec();
    let hardware_path = save_hardware_summary(&state.session_dir, &spec)?;
    if refresh || all {
        let cached = refresh_recommendation_capabilities(state, all, include_cloud).await?;
        println!(
            "  {GREEN}✓{RESET} {DIM}refreshed {} capability record(s).{RESET}",
            cached
        );
    }

    let candidates = collect_recommendation_candidates(state, &spec, all, include_cloud).await?;
    let recommendations = recommend_models(&spec, candidates, include_cloud);
    if recommendations.is_empty() {
        println!("  {DIM}No model candidates found. Start a local backend, then rerun /recommend refresh.{RESET}");
        return Ok(());
    }

    print_recommendations(&spec, &recommendations);
    println!(
        "  {DIM}hardware summary cached → {}{RESET}",
        hardware_path.display()
    );

    let best = recommendations
        .first()
        .expect("checked recommendations is not empty");
    if !best.installed {
        println!(
            "  {YELLOW}!{RESET} {DIM}top recommendation is not installed: {}{RESET}",
            backend_model_hint(best.backend, &best.model)
        );
    }

    if !apply {
        println!(
            "  {DIM}Run /recommend apply to switch this session to the top recommendation.{RESET}"
        );
        return Ok(());
    }

    let env_backend = backend(best.backend);
    if env_backend.base_url != best.base_url {
        println!(
            "  {YELLOW}!{RESET} {DIM}recommended URL was {}, current env resolves to {}{RESET}",
            best.base_url, env_backend.base_url
        );
    }
    apply_recommendation_to_config(&mut state.config, best);
    state.rebuild_client()?;
    state.resolve_model();
    println!(
        "  {GREEN}✓{RESET} {DIM}active session recommendation applied → {} · {} · profile {}{RESET}",
        state.config.backend.as_str(),
        state.model,
        state.config.profile
    );
    Ok(())
}

fn last_user_prompt(state: &AppState) -> Option<String> {
    state
        .messages
        .iter()
        .rev()
        .find_map(|m| m.user_text().map(str::to_string))
}

fn parse_eval_model(spec: &str, state: &AppState) -> (BackendDescriptor, String) {
    if let Some((prefix, model)) = spec.split_once(':') {
        if let Some(name) = BackendName::parse(prefix) {
            return (backend(name), model.to_string());
        }
    }
    (state.backend.clone(), spec.to_string())
}

async fn eval_once(
    state: &AppState,
    backend_desc: &BackendDescriptor,
    model: &str,
    prompt: &str,
    tools_on: bool,
) -> Result<String> {
    let messages = vec![ChatMessage::User {
        content: prompt.to_string(),
    }];
    let tool_defs = if tools_on {
        let tools = build_tools(&state.config);
        to_openai_tools(&tools)
    } else {
        Vec::new()
    };
    let req = ChatRequest {
        model,
        messages: &messages,
        tools: if tool_defs.is_empty() {
            None
        } else {
            Some(&tool_defs)
        },
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: false,
        }),
        max_tokens: None,
    };
    let mut out = String::new();
    stream_chat(&state.http, backend_desc, &req, None, |chunk| {
        if let Some(choice) = chunk.choices.first() {
            if let Some(content) = &choice.delta.content {
                out.push_str(content);
            }
        }
    })
    .await?;
    Ok(out)
}

async fn cmd_eval(args: &str, state: &AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let (prompts, model_specs) = if let Some(first) = parts.first() {
        if Path::new(first).exists() {
            let text = fs::read_to_string(first)?;
            let prompts: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_string)
                .collect();
            let models = parts
                .get(1)
                .map(|s| s.split(',').map(str::to_string).collect())
                .unwrap_or_else(|| vec![state.model.clone()]);
            (prompts, models)
        } else {
            let prompts = last_user_prompt(state)
                .map(|p| vec![p])
                .ok_or_else(|| anyhow!("No prompt file found and no prior user message"))?;
            let models = first.split(',').map(str::to_string).collect();
            (prompts, models)
        }
    } else {
        let prompts = last_user_prompt(state)
            .map(|p| vec![p])
            .ok_or_else(|| anyhow!("No prior user message to evaluate"))?;
        (prompts, vec![state.model.clone()])
    };
    if prompts.is_empty() {
        println!("  {DIM}No prompts to evaluate.{RESET}");
        return Ok(());
    }
    let eval_dir = Path::new(&state.session_dir).join("evals");
    fs::create_dir_all(&eval_dir)?;
    let id = Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ").to_string();
    let mut json_results = Vec::new();
    let mut md = String::from("# Small Harness Eval\n\n");
    for spec in model_specs {
        let (backend_desc, model) = parse_eval_model(&spec, state);
        validate(&backend_desc)?;
        for prompt in &prompts {
            for tools_on in [false, true] {
                println!(
                    "  {DIM}eval {} · tools={} · {}{RESET}",
                    backend_desc.name.as_str(),
                    tools_on,
                    model
                );
                let start = Instant::now();
                let result = eval_once(state, &backend_desc, &model, prompt, tools_on).await;
                let elapsed_ms = start.elapsed().as_millis();
                let output = result.unwrap_or_else(|e| format!("ERROR: {e}"));
                json_results.push(serde_json::json!({
                    "backend": backend_desc.name.as_str(),
                    "model": model.clone(),
                    "toolsOn": tools_on,
                    "prompt": prompt,
                    "output": output,
                    "elapsedMs": elapsed_ms,
                }));
                md.push_str(&format!(
                    "## {} · {} · tools={}\n\n**Prompt**\n\n{}\n\n**Output**\n\n{}\n\n",
                    backend_desc.name.as_str(),
                    model,
                    tools_on,
                    prompt,
                    json_results
                        .last()
                        .and_then(|v| v.get("output"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ));
            }
        }
    }
    let json_path = eval_dir.join(format!("{id}.json"));
    let md_path = eval_dir.join(format!("{id}.md"));
    fs::write(&json_path, serde_json::to_string_pretty(&json_results)?)?;
    fs::write(&md_path, md)?;
    println!(
        "  {GREEN}✓{RESET} {DIM}eval saved → {} and {}{RESET}",
        json_path.display(),
        md_path.display()
    );
    Ok(())
}

fn cmd_batch(args: &str, state: &AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        println!("  {DIM}Usage: /batch [preview|apply] [operations.json]{RESET}");
        println!("  {DIM}  /batch preview ops.json  - Show what would change{RESET}");
        println!("  {DIM}  /batch apply ops.json    - Execute the operations{RESET}");
        return Ok(());
    }

    let action = parts[0];
    let json_path = if parts.len() > 1 {
        PathBuf::from(parts[1])
    } else {
        println!(
            "  {YELLOW}!{RESET} {DIM}Please provide a path to the operations JSON file.{RESET}"
        );
        return Ok(());
    };

    if !json_path.exists() {
        println!(
            "  {RED}✗{RESET} {DIM}Operations file not found: {}{RESET}",
            json_path.display()
        );
        return Ok(());
    }

    let json_content = fs::read_to_string(&json_path)?;
    let operations: Vec<BatchEditOperation> = match serde_json::from_str(&json_content) {
        Ok(ops) => ops,
        Err(e) => {
            println!("  {RED}✗{RESET} {DIM}Failed to parse operations: {e}{RESET}");
            return Ok(());
        }
    };

    match action {
        "preview" => {
            let preview = preview_batch_operations(&operations);
            println!("  {DIM}Batch Operations Preview{RESET}");
            println!("  {DIM}Total files: {}{RESET}", preview.total_files);
            println!(
                "  {DIM}Estimated changes: {}{RESET}",
                preview.estimated_changes
            );
            println!();
            for (i, op) in preview.operations.iter().enumerate() {
                println!("  {CYAN}{}. {}{RESET}", i + 1, op.file_path);
                match &op.operation {
                    EditOperation::Replace { old_string, .. } => {
                        println!(
                            "    {DIM}Replace: {}{RESET}",
                            truncate_string(old_string, 60)
                        );
                    }
                    EditOperation::Insert { position, .. } => {
                        println!("    {DIM}Insert: {:?}{RESET}", position);
                    }
                    EditOperation::Delete { pattern } => {
                        println!("    {DIM}Delete: {}{RESET}", truncate_string(pattern, 60));
                    }
                }
            }
            let workspace_root = Path::new(&state.config.workspace_root);
            let dry_run = execute_batch_operations(&operations, workspace_root, true)?;
            if dry_run.failed.is_empty() {
                println!(
                    "  {GREEN}✓{RESET} {DIM}batch is valid; {} file(s) would change{RESET}",
                    dry_run.skipped.len()
                );
            } else {
                println!(
                    "  {RED}✗{RESET} {DIM}batch has {} validation error(s){RESET}",
                    dry_run.failed.len()
                );
                for fail in &dry_run.failed {
                    println!("    {RED}{}: {}{RESET}", fail.file_path, fail.error);
                }
            }
        }
        "apply" => {
            println!("  {DIM}Applying batch operations...{RESET}");
            let workspace_root = Path::new(&state.config.workspace_root);
            let result = execute_batch_operations(&operations, workspace_root, false)?;

            if !result.successful.is_empty() {
                println!(
                    "  {GREEN}✓{RESET} {DIM}Successfully applied {} file(s){RESET}",
                    result.successful.len()
                );
                for file in &result.successful {
                    println!("    {DIM}{}{RESET}", file);
                }
            }

            if !result.failed.is_empty() {
                println!(
                    "  {RED}✗{RESET} {DIM}Failed on {} file(s){RESET}",
                    result.failed.len()
                );
                for fail in &result.failed {
                    println!("    {RED}{}: {}{RESET}", fail.file_path, fail.error);
                }
            }

            if !result.skipped.is_empty() {
                println!(
                    "  {YELLOW}!{RESET} {DIM}Skipped {} file(s){RESET}",
                    result.skipped.len()
                );
                for file in &result.skipped {
                    println!("    {DIM}{}{RESET}", file);
                }
            }
        }
        _ => {
            println!("  {DIM}Usage: /batch [preview|apply] [operations.json]{RESET}");
        }
    }

    Ok(())
}

fn cmd_refactor(args: &str, state: &AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        println!("  {DIM}Usage: /refactor [references|related] <file_path>{RESET}");
        println!("  {DIM}  /refactor references src/main.rs  - Find cross-file references{RESET}");
        println!("  {DIM}  /refactor related src/main.rs     - Find related files{RESET}");
        return Ok(());
    }

    let action = parts[0];
    if parts.len() < 2 {
        println!("  {YELLOW}!{RESET} {DIM}Please provide a file path.{RESET}");
        return Ok(());
    }

    let file_path = parts[1];

    match action {
        "references" => {
            let references = find_cross_file_references(&state.config, file_path)?;
            if references.is_empty() {
                println!(
                    "  {DIM}No cross-file references found for {}{RESET}",
                    file_path
                );
            } else {
                println!("  {DIM}Cross-file references for {}{RESET}", file_path);
                for ref_info in references {
                    println!(
                        "    {CYAN}{}{RESET} {DIM}→{}{RESET}",
                        ref_info.from_file, ref_info.to_file
                    );
                    println!(
                        "      {DIM}type: {}, line: {}{RESET}",
                        ref_info.reference_type, ref_info.line
                    );
                }
            }
        }
        "related" => {
            let related = find_related_files(&state.config, file_path)?;
            if related.is_empty() {
                println!("  {DIM}No related files found for {}{RESET}", file_path);
            } else {
                println!("  {DIM}Related files for {}{RESET}", file_path);
                for file in related {
                    println!("    {CYAN}{}{RESET}", file);
                }
            }
        }
        _ => {
            println!("  {DIM}Usage: /refactor [references|related] <file_path>{RESET}");
        }
    }

    Ok(())
}

fn cmd_test(args: &str, state: &AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        println!("  {DIM}Usage: /test [discover|run|smart] [args]{RESET}");
        println!("  {DIM}  /test discover              - Auto-detect test framework and list tests{RESET}");
        println!("  {DIM}  /test run [test_pattern]   - Run all tests or matching pattern{RESET}");
        println!("  {DIM}  /test smart                 - Run tests based on changed files{RESET}");
        return Ok(());
    }

    let action = parts[0];

    match action {
        "discover" => match discover_tests(&state.config.workspace_root) {
            Ok(discovery) => {
                println!("  {DIM}Test Discovery{RESET}");
                println!("  {DIM}Framework: {}{RESET}", discovery.framework);
                println!(
                    "  {DIM}Test files found: {}{RESET}",
                    discovery.test_files.len()
                );
                for test_file in &discovery.test_files {
                    println!("    {CYAN}{}{RESET}", test_file);
                }
                if let Some(command) = discovery.run_command {
                    println!("  {DIM}Run command: {}{RESET}", command);
                }
            }
            Err(e) => {
                println!("  {RED}✗{RESET} {DIM}Test discovery failed: {e}{RESET}");
            }
        },
        "run" => {
            let pattern = if parts.len() > 1 {
                Some(parts[1])
            } else {
                None
            };
            match run_tests(&state.config.workspace_root, pattern) {
                Ok(results) => {
                    println!("  {DIM}Test Results{RESET}");
                    println!("  {DIM}Total: {}{RESET}", results.total);
                    println!("  {DIM}Passed: {}{RESET}", results.passed);
                    println!("  {DIM}Failed: {}{RESET}", results.failed);
                    println!("  {DIM}Skipped: {}{RESET}", results.skipped);
                    if !results.failures.is_empty() {
                        println!();
                        println!("  {RED}Failures:{RESET}");
                        for failure in &results.failures {
                            println!("    {RED}{}{RESET}", failure);
                        }
                    }
                    if results.exit_code != 0 {
                        println!();
                        println!(
                            "  {RED}✗{RESET} {DIM}Tests failed with exit code {}{RESET}",
                            results.exit_code
                        );
                    } else {
                        println!();
                        println!("  {GREEN}✓{RESET} {DIM}All tests passed{RESET}");
                    }
                }
                Err(e) => {
                    println!("  {RED}✗{RESET} {DIM}Test execution failed: {e}{RESET}");
                }
            }
        }
        "smart" => match smart_test_selection(&state.config.workspace_root) {
            Ok(selected_tests) => {
                println!("  {DIM}Smart Test Selection{RESET}");
                println!(
                    "  {DIM}Based on changed files, {} test(s) selected{RESET}",
                    selected_tests.len()
                );
                for test in &selected_tests {
                    println!("    {CYAN}{}{RESET}", test);
                }
                if !selected_tests.is_empty() {
                    println!();
                    println!("  {DIM}Running selected tests...{RESET}");
                    match run_selected_tests(&state.config.workspace_root, &selected_tests) {
                        Ok(results) => {
                            println!(
                                "  {DIM}Total: {} Passed: {} Failed: {}{RESET}",
                                results.total, results.passed, results.failed
                            );
                        }
                        Err(e) => {
                            println!("  {RED}✗{RESET} {DIM}Test execution failed: {e}{RESET}");
                        }
                    }
                }
            }
            Err(e) => {
                println!("  {RED}✗{RESET} {DIM}Smart test selection failed: {e}{RESET}");
            }
        },
        _ => {
            println!("  {DIM}Usage: /test [discover|run|smart] [args]{RESET}");
        }
    }

    Ok(())
}

async fn cmd_prompt(args: &str, state: &mut AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        println!("  {DIM}Usage: /prompt [save|list|run|template|builtin|delete|export|import] [args]{RESET}");
        println!(
            "  {DIM}  /prompt save <name> [text]    - Save text or the last user prompt{RESET}"
        );
        println!("  {DIM}  /prompt list                  - List saved prompts{RESET}");
        println!("  {DIM}  /prompt run <name> [k=v]      - Run a saved or built-in prompt{RESET}");
        println!("  {DIM}  /prompt template <name>       - Create parameterized template{RESET}");
        println!("  {DIM}  /prompt builtin                - List built-in prompts{RESET}");
        println!("  {DIM}  /prompt builtin <name>        - Show a built-in prompt{RESET}");
        println!("  {DIM}  /prompt delete <name>         - Delete a saved prompt{RESET}");
        println!("  {DIM}  /prompt export <path>         - Export all prompts to JSON{RESET}");
        println!("  {DIM}  /prompt import <path>         - Import prompts from JSON{RESET}");
        return Ok(());
    }

    let action = parts[0];
    let library = PromptLibrary::new();

    match action {
        "save" => {
            if parts.len() < 2 {
                println!("  {YELLOW}!{RESET} {DIM}Please provide a name for the prompt.{RESET}");
                return Ok(());
            }
            let name = parts[1];
            let content = args
                .splitn(3, ' ')
                .nth(2)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .or_else(|| latest_user_prompt(&state.messages).map(str::to_string));
            let Some(content) = content else {
                println!("  {YELLOW}!{RESET} {DIM}No prompt text provided and no prior user prompt found.{RESET}");
                return Ok(());
            };
            save_prompt(&state.config.session_dir, name, &content)?;
            println!("  {GREEN}✓{RESET} {DIM}Prompt '{}' saved{RESET}", name);
        }
        "list" => {
            let user_prompts = list_prompts(&state.config.session_dir)?;

            if user_prompts.is_empty() {
                println!("  {DIM}No saved prompts found.{RESET}");
            } else {
                println!("  {DIM}Saved prompts:{RESET}");
                for name in &user_prompts {
                    println!("    {CYAN}{}{RESET}", name);
                }
            }

            println!();
            println!("  {DIM}Built-in prompts:{RESET}");
            for template in library.list() {
                println!(
                    "    {CYAN}{}{RESET} - {}",
                    template.name, template.description
                );
            }
        }
        "run" => {
            if parts.len() < 2 {
                println!("  {YELLOW}!{RESET} {DIM}Please provide a prompt name.{RESET}");
                return Ok(());
            }
            let name = parts[1];

            if name.starts_with("builtin:") {
                let builtin_name = name.strip_prefix("builtin:").unwrap();
                if let Some(template) = library.get(builtin_name) {
                    let variables = parse_prompt_variables(&parts[2..]);
                    let missing: Vec<&str> = template
                        .variables
                        .iter()
                        .map(String::as_str)
                        .filter(|name| !variables.contains_key(*name))
                        .collect();
                    if !missing.is_empty() {
                        println!(
                            "  {YELLOW}!{RESET} {DIM}Missing variable(s): {}{RESET}",
                            missing.join(", ")
                        );
                        println!(
                            "  {DIM}Pass variables as key=value after the prompt name.{RESET}"
                        );
                        return Ok(());
                    }
                    let content = library.render(builtin_name, &variables)?;
                    run_prompt_content(&content, state).await?;
                } else {
                    println!(
                        "  {RED}✗{RESET} {DIM}Built-in prompt not found: {}{RESET}",
                        builtin_name
                    );
                }
            } else {
                match load_prompt(&state.config.session_dir, name) {
                    Ok(content) => {
                        run_prompt_content(&content, state).await?;
                    }
                    Err(_) => {
                        println!("  {RED}✗{RESET} {DIM}Prompt not found: {}{RESET}", name);
                    }
                }
            }
        }
        "template" => {
            if parts.len() < 2 {
                println!("  {YELLOW}!{RESET} {DIM}Please provide a template name.{RESET}");
                return Ok(());
            }
            let name = parts[1];
            let template_content = r#"# Parameterized Template: {{name}}
# Variables: {{var1}}, {{var2}}
# Use {{variable}} syntax for parameters

Your prompt template goes here.
Use double curly braces {{}} for variable placeholders."#;
            save_prompt(&state.config.session_dir, name, template_content)?;
            println!("  {GREEN}✓{RESET} {DIM}Template '{}' saved{RESET}", name);
        }
        "builtin" => {
            if parts.len() < 2 {
                println!("  {DIM}Built-in prompts:{RESET}");
                for template in library.list() {
                    println!(
                        "    {CYAN}{}{RESET} - {}",
                        template.name, template.description
                    );
                }
                println!();
                println!("  {DIM}Use: /prompt run builtin:<name>{RESET}");
            } else {
                let name = parts[1];
                if let Some(template) = library.get(name) {
                    println!("  {DIM}Built-in prompt: {}{RESET}", template.name);
                    println!("  {DIM}Description: {}{RESET}", template.description);
                    println!();
                    println!("{}", template.content);
                    if !template.variables.is_empty() {
                        println!();
                        println!("  {DIM}Variables: {}{RESET}", template.variables.join(", "));
                    }
                } else {
                    println!(
                        "  {RED}✗{RESET} {DIM}Built-in prompt not found: {}{RESET}",
                        name
                    );
                }
            }
        }
        "delete" => {
            if parts.len() < 2 {
                println!("  {YELLOW}!{RESET} {DIM}Please provide a prompt name.{RESET}");
                return Ok(());
            }
            let name = parts[1];
            match delete_prompt(&state.config.session_dir, name) {
                Ok(_) => println!("  {GREEN}✓{RESET} {DIM}Prompt '{}' deleted{RESET}", name),
                Err(e) => println!("  {RED}✗{RESET} {DIM}{}{RESET}", e),
            }
        }
        "export" => {
            if parts.len() < 2 {
                println!("  {YELLOW}!{RESET} {DIM}Please provide an export path.{RESET}");
                return Ok(());
            }
            let export_path = PathBuf::from(parts[1]);
            match export_prompts(&state.config.session_dir, &export_path) {
                Ok(_) => println!(
                    "  {GREEN}✓{RESET} {DIM}Prompts exported to {}{RESET}",
                    export_path.display()
                ),
                Err(e) => println!("  {RED}✗{RESET} {DIM}{}{RESET}", e),
            }
        }
        "import" => {
            if parts.len() < 2 {
                println!("  {YELLOW}!{RESET} {DIM}Please provide an import path.{RESET}");
                return Ok(());
            }
            let import_path = PathBuf::from(parts[1]);
            match import_prompts(&state.config.session_dir, &import_path) {
                Ok(count) => println!("  {GREEN}✓{RESET} {DIM}Imported {} prompt(s){RESET}", count),
                Err(e) => println!("  {RED}✗{RESET} {DIM}{}{RESET}", e),
            }
        }
        _ => {
            println!("  {DIM}Usage: /prompt [save|list|run|template|builtin|delete|export|import] [args]{RESET}");
        }
    }

    Ok(())
}

fn latest_user_prompt(messages: &[ChatMessage]) -> Option<&str> {
    messages.iter().rev().find_map(ChatMessage::user_text)
}

fn parse_prompt_variables(parts: &[&str]) -> HashMap<String, String> {
    parts
        .iter()
        .filter_map(|part| part.split_once('='))
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

async fn run_prompt_content(content: &str, state: &mut AppState) -> Result<()> {
    let needs_system = !matches!(state.messages.first(), Some(ChatMessage::System { .. }));
    if needs_system {
        let system = ChatMessage::System {
            content: state.config.render_system_prompt_for_tools(&[]),
        };
        save_message(&state.session_path, &system)?;
        state.messages.insert(0, system);
    }

    let user_msg = ChatMessage::User {
        content: content.to_string(),
    };
    state.messages.push(user_msg.clone());
    save_message(&state.session_path, &user_msg)?;

    let request_messages = state.messages.clone();
    let req = ChatRequest {
        model: &state.model,
        messages: &request_messages,
        tools: None,
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        max_tokens: None,
    };

    println!(
        "  {DIM}running prompt with {} · {}{RESET}",
        state.config.backend.as_str(),
        state.model
    );
    let mut assistant = String::new();
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    let mut out = std::io::stdout();
    stream_chat(&state.http, &state.backend, &req, None, |chunk| {
        if let Some(usage) = chunk.usage {
            input_tokens = usage.prompt_tokens;
            output_tokens = usage.completion_tokens;
        }
        if let Some(choice) = chunk.choices.first() {
            if let Some(delta) = &choice.delta.content {
                assistant.push_str(delta);
                let _ = out.write_all(delta.as_bytes());
                let _ = out.flush();
            }
        }
    })
    .await?;
    println!();

    let assistant_msg = ChatMessage::Assistant {
        content: Some(assistant),
        tool_calls: Vec::new(),
    };
    state.messages.push(assistant_msg.clone());
    save_message(&state.session_path, &assistant_msg)?;
    state.total_in += input_tokens;
    state.total_out += output_tokens;
    Ok(())
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::BackendDescriptor;
    use std::process::Command;

    fn test_state(root: &Path) -> AppState {
        let mut config = AgentConfig {
            workspace_root: root.display().to_string(),
            session_dir: root.join(".sessions").display().to_string(),
            ..Default::default()
        };
        config.project_memory.max_injected_bytes = 1024;
        AppState {
            http: reqwest::Client::new(),
            backend: backend(config.backend),
            model: "test-model".into(),
            messages: Vec::new(),
            session_dir: config.session_dir.clone(),
            session_path: root.join(".sessions/test.jsonl"),
            total_in: 0,
            total_out: 0,
            context_guard_notice: None,
            config,
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test User"]);
        fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(dir, &["add", "README.md"]);
        git(dir, &["commit", "-m", "initial"]);
    }

    #[test]
    fn detects_inline_tool_json_for_doctor_noop() {
        assert!(looks_like_inline_tool_json(
            r#"{"name":"doctor_noop","arguments":{"ok":true}}"#
        ));
        assert!(!looks_like_inline_tool_json(
            r#"{"name":"other_tool","arguments":{"ok":true}}"#
        ));
        assert!(!looks_like_inline_tool_json("plain text"));
    }

    #[test]
    fn llama_cpp_warning_mentions_jinja_when_tool_calls_missing() {
        let row = DoctorCapabilityRow {
            backend: "llamacpp".into(),
            base_url: "http://localhost:8080/v1".into(),
            model: "gpt-3.5-turbo".into(),
            models: ProbeStatus::ok("1 model"),
            streaming: ProbeStatus::ok("stream ok"),
            usage_chunks: ProbeStatus::ok("usage ok"),
            tool_calls: ProbeStatus::fail("missing"),
            inline_tool_json: ProbeStatus::fail("missing"),
            warning: None,
        };
        assert!(doctor_warning(&row).unwrap().contains("--jinja"));
    }

    #[test]
    fn memory_commands_toggle_and_persist_notes() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path());

        cmd_memory("off", &mut state);
        assert!(!state.config.project_memory.enabled);
        cmd_memory("on", &mut state);
        assert!(state.config.project_memory.enabled);

        cmd_remember("Entry point is src/main.rs", &state).unwrap();
        let notes = load_project_notes(&state.config).unwrap();
        assert_eq!(notes.len(), 1);
        cmd_forget(&notes[0].id, &state).unwrap();
        assert!(load_project_notes(&state.config).unwrap().is_empty());
    }

    #[test]
    fn index_command_builds_maps_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let state = test_state(dir.path());

        cmd_index("", &state).unwrap();
        assert!(load_project_index(&state.config).unwrap().is_some());
        cmd_map("main", &state).unwrap();
        cmd_index("clear", &state).unwrap();
        assert!(load_project_index(&state.config).unwrap().is_none());
    }

    #[test]
    fn parses_handoff_export_and_cloud_args() {
        let args = parse_handoff_args("export .sessions/handoff/manual.md --cloud").unwrap();
        assert!(args.export);
        assert!(args.allow_cloud);
        assert_eq!(
            args.explicit_path.as_deref(),
            Some(Path::new(".sessions/handoff/manual.md"))
        );
        assert!(parse_handoff_args("unexpected").is_none());
    }

    #[tokio::test]
    async fn handoff_noops_without_changes_or_ahead_commits() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let state = test_state(dir.path());

        cmd_handoff("export", &state).await.unwrap();

        assert!(!dir.path().join(".sessions/handoff").exists());
    }

    #[tokio::test]
    async fn handoff_export_writes_fallback_when_model_fails() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("README.md"), "changed\n").unwrap();
        let out_path = dir.path().join("handoff.md");
        let mut state = test_state(dir.path());
        state.backend = BackendDescriptor {
            name: BackendName::Ollama,
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "test".into(),
            is_local: true,
        };

        cmd_handoff(&format!("export {}", out_path.display()), &state)
            .await
            .unwrap();

        let body = fs::read_to_string(out_path).unwrap();
        assert!(body.contains("## Commit Message"));
        assert!(body.contains("## Changelog Bullets"));
        assert!(body.contains("## X Post"));
        assert!(body.contains("## Testing"));
        assert!(body.contains("model draft failed"));
    }
}

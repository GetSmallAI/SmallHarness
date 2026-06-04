use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::agent::to_openai_tools;
use crate::agent_eval::{builtin_fixtures, render_agent_eval_markdown, run_agent_eval};
use crate::app_state::AppState;
use crate::backends::{backend, default_model, validate, BackendDescriptor, BackendName};
use crate::batch_operations::{
    execute_batch_operations, find_cross_file_references, find_related_files,
    preview_batch_operations, BatchEditOperation, EditOperation,
};
use crate::budget::{format_bytes, measure_prompt_budget};
use crate::capabilities::{
    self, best_record, recommended_tool_selection, record_score, sorted_records,
    warmup_recommended, BenchmarkStats, CapabilityRecord, CapabilityStatus,
};
use crate::catalog;
use crate::config::{is_tool_name, OperatorMode, ToolSelection, ALL_TOOL_NAMES};
use crate::context_guard::{
    compact_session, context_status_lines, extract_conversation_summary, merge_system_prompt,
    CompactMethod, CompactSessionContext,
};
use crate::fix_loop::{parse_fix_args, run_fix_loop};
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
use crate::playground::{
    print_play_list, print_scorecard, restore_play_session, run_play_battle, run_play_fixture,
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
    delete_session, list_sessions, load_messages, load_session, load_session_metadata,
    render_markdown, resolve_session_path, save_message, save_session_metadata, search_sessions,
    set_session_title, SessionEntry,
};
use crate::session_paths::{apply_path_session_state, PathStore, DEFAULT_PATH_ID};
use crate::shipcheck::{
    collect_shipcheck, collect_shipcheck_with_tests, default_export_path, file_status_label,
    render_markdown as render_shipcheck_markdown, ShipcheckSnapshot,
};
use crate::test_integration::{
    discover_tests, run_selected_tests, run_tests, smart_test_selection,
};
use crate::tools::{build_tools, build_tools_for_names, select_tool_names};
use crate::warmup::warmup;

// Command groups split out of this file. `dispatch` (below) stays the single
// router; these modules hold the cmd_* handlers and their private helpers.
mod doctor;
mod workflow;
use doctor::*;
use workflow::*;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";

pub const COMMANDS: &[(&str, &str)] = &[
    ("/undo", "Revert the last agent turn's file mutations"),
    (
        "/checkpoints",
        "Show or toggle turn checkpoints (on, off, status)",
    ),
    (
        "/path",
        "Fork, switch, diff, pick, or drop parallel session paths",
    ),
    ("/paths", "List saved session paths"),
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
        "/auth",
        "Manage API keys for cloud providers (list, set <provider>, clear <provider>)",
    ),
    (
        "/image",
        "Attach an image to the next user prompt (vision-capable models only)",
    ),
    (
        "/reasoning",
        "Toggle the streaming reasoning panel (on, off, status)",
    ),
    (
        "/backend",
        "Switch backend (ollama, lm-studio, mlx, llamacpp, openrouter)",
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
    (
        "/context",
        "Show or update context limits and auto-guard status",
    ),
    ("/compact", "Summarize or trim older conversation turns"),
    (
        "/doctor",
        "Probe backend/tools/env; subcommands: recommend, bench, models, autotune",
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
    (
        "/play",
        "Try bundled agent demos (fix-failing-test, add-feature, battle, exit)",
    ),
    (
        "/fix",
        "Fix-until-green loop on your repo (smart tests, --attempts, --yolo)",
    ),
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
        "/undo" => cmd_undo(&args, state)?,
        "/checkpoints" => cmd_checkpoints(&args, state),
        "/path" => cmd_path(&args, state).await?,
        "/paths" => cmd_paths(state)?,
        "/config" => cmd_config(state),
        "/mode" => cmd_mode(&args, state),
        "/shipcheck" => cmd_shipcheck(&args, state)?,
        "/handoff" => cmd_handoff(&args, state).await?,
        "/session" => cmd_session(&args, state)?,
        "/sessions" => cmd_sessions(&args, state)?,
        "/resume" => cmd_resume(&args, state)?,
        "/export" => cmd_export(&args, state)?,
        "/auth" => cmd_auth(&args).await?,
        "/image" => cmd_image(&args, state),
        "/reasoning" => cmd_reasoning(&args, state),
        "/backend" => cmd_backend(&args, state).await?,
        "/model" => cmd_model(&args, state).await?,
        "/tools" => cmd_tools(&args, state),
        "/compare" => cmd_compare(&args, state).await?,
        "/context" => cmd_context(&args, state),
        "/compact" => cmd_compact(&args, state).await?,
        "/doctor" => cmd_doctor(&args, state).await?,
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
        "/play" => cmd_play(&args, state).await?,
        "/fix" => cmd_fix(&args, state).await?,
        // These model-tuning commands were folded into `/doctor` subcommands.
        "/bench" => redirect_to_doctor("/bench", "bench"),
        "/capabilities" => redirect_to_doctor("/capabilities", "models"),
        "/autotune" => redirect_to_doctor("/autotune", "autotune"),
        "/recommend" => redirect_to_doctor("/recommend", "recommend"),
        other => {
            println!("  {DIM}Unknown command: {other}. Type /help.{RESET}");
        }
    }
    Ok(())
}

/// One-liner shown when someone types an old top-level command that is now a
/// `/doctor` subcommand. Keeps muscle memory working without re-listing the
/// commands in `/help`.
fn redirect_to_doctor(old: &str, sub: &str) {
    println!("  {DIM}{old} is now {CYAN}/doctor {sub}{RESET}{DIM}. Run that instead.{RESET}");
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
    let model = default_model(&backend_desc, config.model_override.as_deref());
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
    if state.play_session.is_some() {
        let _ = restore_play_session(state);
    }
    state.messages.clear();
    state.conversation_summary = None;
    state.context_guard_notice = None;
    state.checkpoint_stack =
        crate::turn_checkpoint::CheckpointStack::new(state.config.checkpoints.limits());
    state.total_in = 0;
    state.total_out = 0;
    state.session_usd = 0.0;
    state.session_cost_has_unknown = false;
    state.reset_session();
    println!("  {GREEN}✓{RESET} {DIM}New session started.{RESET}");
}

fn ensure_path_ops_allowed(state: &AppState) -> Result<()> {
    if state.in_play_session() {
        anyhow::bail!("cannot use /path during a /play session — /play exit first");
    }
    Ok(())
}

fn cmd_paths(state: &AppState) -> Result<()> {
    if !state.paths_enabled() {
        println!("  {DIM}session paths are disabled in config{RESET}");
        return Ok(());
    }
    let active = state.path_store.active_id();
    if state.path_store.registry.paths.is_empty() {
        println!(
            "  {DIM}path{RESET}           {CYAN}{active}{RESET} {DIM}(only path — /path fork to branch){RESET}"
        );
        return Ok(());
    }
    println!(
        "  {DIM}active{RESET}         {CYAN}{}{RESET} · {} path(s) · {} stored",
        active,
        state.path_store.path_count(),
        format_bytes(state.path_store.total_storage_bytes() as usize)
    );
    for record in &state.path_store.registry.paths {
        let marker = if record.id == active {
            format!("{GREEN}*{RESET} ")
        } else {
            "  ".to_string()
        };
        println!(
            "  {marker}{CYAN}{}{RESET} {DIM}msgs={} files={} updated={}{RESET}",
            record.id, record.message_count, record.file_count, record.updated_at
        );
    }
    Ok(())
}

async fn cmd_path(args: &str, state: &mut AppState) -> Result<()> {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "status" {
        cmd_path_status(state);
        return Ok(());
    }
    let mut parts = trimmed.splitn(2, ' ');
    let sub = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match sub {
        "fork" => {
            ensure_path_ops_allowed(state)?;
            let name = if rest.is_empty() { None } else { Some(rest) };
            let root = state.workspace_root();
            let session_path = state.session_path.clone();
            let current = PathStore::capture_state(state, &root)?;
            let (new_id, new_state) = state.path_store.fork(current, &session_path, name, &root)?;
            let transcript = state.path_store.transcript_path(&new_id);
            apply_path_session_state(state, &new_state, &transcript);
            let _ = state.save_active_path_metadata();
            let parent = state
                .path_store
                .registry
                .paths
                .iter()
                .find(|p| p.id == new_id)
                .and_then(|p| p.parent_id.clone())
                .unwrap_or_else(|| DEFAULT_PATH_ID.to_string());
            let notice = format!(
                "Forked to path '{new_id}' from '{parent}' at message {}.",
                state.messages.len()
            );
            state.messages.push(ChatMessage::System {
                content: notice.clone(),
            });
            println!(
                "  {GREEN}✓{RESET} {DIM}forked to path{RESET} {CYAN}{new_id}{RESET} {DIM}— continue here, /path switch to compare{RESET}"
            );
        }
        "switch" => {
            ensure_path_ops_allowed(state)?;
            if rest.is_empty() {
                println!("  {DIM}Usage: /path switch <name>{RESET}");
                return Ok(());
            }
            let root = state.workspace_root();
            let current = PathStore::capture_state(state, &root)?;
            let (path_state, report) = state.path_store.switch_to(rest, current, &root)?;
            let transcript = state
                .path_store
                .transcript_path(state.path_store.active_id());
            apply_path_session_state(state, &path_state, &transcript);
            let _ = state.save_active_path_metadata();
            println!(
                "  {GREEN}✓{RESET} {DIM}switched to path{RESET} {CYAN}{}{RESET}",
                state.path_store.active_id()
            );
            if !report.restored.is_empty() || !report.removed.is_empty() {
                println!(
                    "  {DIM}restored {} · removed {}{RESET}",
                    report.restored.len(),
                    report.removed.len()
                );
            }
            if report.is_partial() {
                println!(
                    "  {YELLOW}!{RESET} {DIM}partial restore — {} skipped, {} errors{RESET}",
                    report.skipped.len(),
                    report.errors.len()
                );
            }
        }
        "diff" => {
            if rest.is_empty() {
                println!("  {DIM}Usage: /path diff <name>{RESET}");
                return Ok(());
            }
            let diff = state.path_store.diff_with(rest, &state.workspace_root())?;
            if diff.is_empty() {
                println!("  {DIM}No file differences vs path `{rest}`.{RESET}");
            } else {
                for line in diff.lines().take(120) {
                    println!("  {DIM}{line}{RESET}");
                }
                if diff.lines().count() > 120 {
                    println!("  {DIM}…diff truncated for display{RESET}");
                }
            }
        }
        "pick" => {
            ensure_path_ops_allowed(state)?;
            let mut name = rest;
            let mut dry_run = false;
            if name.starts_with("--dry-run") {
                dry_run = true;
                name = name.strip_prefix("--dry-run").unwrap_or("").trim();
            }
            if name.is_empty() {
                println!("  {DIM}Usage: /path pick <name> [--dry-run]{RESET}");
                return Ok(());
            }
            let preview = state
                .path_store
                .pick_from(name, &state.workspace_root(), true)?;
            if preview.files.is_empty() {
                println!("  {DIM}Nothing to pick from path `{name}`.{RESET}");
                return Ok(());
            }
            if dry_run {
                println!(
                    "  {DIM}dry-run would apply {} file(s): {}{RESET}",
                    preview.files.len(),
                    preview.files.join(", ")
                );
                return Ok(());
            }
            let diff = state.path_store.diff_with(name, &state.workspace_root())?;
            if !diff.is_empty() {
                println!();
                for line in diff.lines().take(80) {
                    println!("  {DIM}{line}{RESET}");
                }
                if diff.lines().count() > 80 {
                    println!("  {DIM}…diff truncated for display{RESET}");
                }
                println!();
            }
            println!(
                "  {YELLOW}?{RESET} {DIM}Apply {} file(s) from `{name}`? [y/n]{RESET}",
                preview.files.len()
            );
            let answer = plain_read_line(format!("  {YELLOW}? {RESET}")).await?;
            if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                println!("  {RED}✗{RESET} {DIM}pick cancelled{RESET}");
                return Ok(());
            }
            let result = state
                .path_store
                .pick_from(name, &state.workspace_root(), false)?;
            if result.applied {
                state.path_store.mark_dirty();
                println!(
                    "  {GREEN}✓{RESET} {DIM}picked {} file(s) from `{name}`{RESET}",
                    result.files.len()
                );
            } else if !result.errors.is_empty() {
                println!(
                    "  {RED}✗{RESET} {DIM}pick failed: {}{RESET}",
                    result.errors.join("; ")
                );
            }
        }
        "drop" => {
            ensure_path_ops_allowed(state)?;
            if rest.is_empty() {
                println!("  {DIM}Usage: /path drop <name>{RESET}");
                return Ok(());
            }
            state.path_store.drop_path(rest)?;
            println!("  {GREEN}✓{RESET} {DIM}dropped path `{rest}`{RESET}");
        }
        other => {
            println!(
                "  {DIM}Usage: /path [fork [name] | switch <name> | diff <name> | pick <name> [--dry-run] | drop <name> | status]{RESET} (unknown: {other})"
            );
        }
    }
    Ok(())
}

fn cmd_path_status(state: &AppState) {
    if !state.paths_enabled() {
        println!("  {DIM}paths{RESET}           disabled in config");
        return;
    }
    let count = state.path_store.path_count();
    println!(
        "  {DIM}path{RESET}           {CYAN}{}{RESET}",
        state.path_store.active_id()
    );
    if count > 1 {
        println!(
            "  {DIM}paths{RESET}          {count} · {} stored",
            format_bytes(state.path_store.total_storage_bytes() as usize)
        );
    } else {
        println!(
            "  {DIM}paths{RESET}          1 {DIM}(/path fork to try an alternate approach){RESET}"
        );
    }
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
    println!(
        "  {DIM}checkpoints{RESET}      enabled={} session={} stack={}/{} maxBytes={}",
        state.config.checkpoints.enabled,
        state.checkpoints_enabled,
        state.checkpoint_stack.len(),
        state.checkpoint_stack.limits.max_turns,
        state.config.checkpoints.max_bytes
    );
    if state.paths_enabled() {
        println!(
            "  {DIM}paths{RESET}            enabled={} active={} count={} maxPaths={}",
            state.config.paths.enabled,
            state.path_store.active_id(),
            state.path_store.path_count(),
            state.config.paths.max_paths
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
    if state.session_usd > 0.0 || state.session_cost_has_unknown {
        let prefix = if state.session_cost_has_unknown {
            "≥"
        } else {
            ""
        };
        println!(
            "  {DIM}cost{RESET}      {prefix}{} (sum of catalog-priced turns)",
            catalog::format_usd(state.session_usd)
        );
    }
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
    state.checkpoints_enabled = state.config.checkpoints.enabled;
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
    let action = parts.first().copied();
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
            content: render_handoff_prompt(&context, freshness.as_ref()).into(),
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
    state.path_store =
        PathStore::load(&state.session_dir, &state.session_path, &state.config.paths);
    let metadata = load_session_metadata(&path)?;
    let root = state.workspace_root();
    if let Some((path_state, report)) = state
        .path_store
        .load_resume_state(&root, metadata.active_path_id.as_deref())?
    {
        let transcript = state
            .path_store
            .transcript_path(state.path_store.active_id());
        apply_path_session_state(state, &path_state, &transcript);
        if report.is_partial() {
            println!(
                "  {YELLOW}!{RESET} {DIM}path restore partial — {} skipped, {} errors{RESET}",
                report.skipped.len(),
                report.errors.len()
            );
        }
    }
    let mut updated = metadata.clone();
    updated.active_path_id = Some(state.path_store.active_id().to_string());
    let _ = save_session_metadata(&path, &updated);
    state.conversation_summary = state.messages.first().and_then(|message| match message {
        ChatMessage::System { content } => extract_conversation_summary(content),
        _ => None,
    });
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

async fn cmd_auth(args: &str) -> Result<()> {
    use crate::auth::{auth_file_path, env_var_for, mask_key, AuthStore, KNOWN_PROVIDERS};

    let (action, rest) = match args.split_once(' ') {
        Some((a, r)) => (a.trim(), r.trim()),
        None => (args.trim(), ""),
    };

    match action {
        "" | "list" | "status" => {
            print_auth_status();
            Ok(())
        }
        "set" => {
            if rest.is_empty() {
                println!(
                    "  {DIM}usage: /auth set <provider>  (known: {}){RESET}",
                    KNOWN_PROVIDERS
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return Ok(());
            }
            let provider = rest.to_lowercase();
            if env_var_for(&provider).is_none() {
                println!("  {RED}✗{RESET} {DIM}unknown provider: {provider}{RESET}");
                return Ok(());
            }
            let key = plain_read_line(format!(
                "  {DIM}Paste {} API key (visible while typing): {RESET}",
                provider
            ))
            .await?
            .trim()
            .to_string();
            if key.is_empty() {
                println!("  {DIM}Cancelled (empty key).{RESET}");
                return Ok(());
            }
            let mut store = AuthStore::load();
            store.set(&provider, &key);
            match store.save() {
                Ok(()) => {
                    if let Some(env_name) = env_var_for(&provider) {
                        std::env::set_var(env_name, &key);
                    }
                    let path = auth_file_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(no path)".into());
                    println!(
                        "  {GREEN}✓{RESET} {DIM}{} →{RESET} {CYAN}{}{RESET} {DIM}(saved to {}){RESET}",
                        provider,
                        mask_key(&key),
                        path
                    );
                }
                Err(e) => println!("  {RED}✗{RESET} {DIM}save failed: {e}{RESET}"),
            }
            Ok(())
        }
        "clear" => {
            if rest.is_empty() {
                println!("  {DIM}usage: /auth clear <provider>{RESET}");
                return Ok(());
            }
            let provider = rest.to_lowercase();
            let mut store = AuthStore::load();
            if !store.clear(&provider) {
                println!("  {DIM}no stored key for {provider}{RESET}");
                return Ok(());
            }
            match store.save() {
                Ok(()) => {
                    println!("  {GREEN}✓{RESET} {DIM}cleared {provider} from auth file{RESET}")
                }
                Err(e) => println!("  {RED}✗{RESET} {DIM}save failed: {e}{RESET}"),
            }
            // Env var stays set for the current process — re-launch to drop it.
            Ok(())
        }
        other => {
            println!(
                "  {RED}✗{RESET} {DIM}unknown subcommand: {other} (try: list, set, clear){RESET}"
            );
            Ok(())
        }
    }
}

fn print_auth_status() {
    use crate::auth::{auth_file_path, mask_key, AuthStore, KNOWN_PROVIDERS};
    let store = AuthStore::load();
    println!("  {DIM}provider     status                  source{RESET}");
    for (provider, env_name) in KNOWN_PROVIDERS {
        let env_val = std::env::var(env_name).unwrap_or_default();
        let (display, source) = if !env_val.is_empty() {
            (mask_key(&env_val), format!("env: {env_name}"))
        } else if let Some(k) = store.get(provider) {
            (mask_key(k), "auth file".into())
        } else {
            ("(not set)".into(), "—".into())
        };
        println!("  {:<12} {:<22}  {DIM}{}{RESET}", provider, display, source);
    }
    if let Some(path) = auth_file_path() {
        println!("  {DIM}file{RESET}     {}", path.display());
    }
}

fn cmd_image(args: &str, state: &mut AppState) {
    let args = args.trim();
    if args.is_empty() || args == "list" {
        if state.pending_image_attachments.is_empty() {
            println!("  {DIM}no images staged. Usage: /image <path>{RESET}");
        } else {
            println!(
                "  {DIM}{} image(s) staged for next turn:{RESET}",
                state.pending_image_attachments.len()
            );
            for (i, url) in state.pending_image_attachments.iter().enumerate() {
                let summary = url.split(';').next().unwrap_or(url);
                println!("  {DIM}{:>2}){RESET} {}", i + 1, summary);
            }
        }
        return;
    }
    if args == "clear" {
        let n = state.pending_image_attachments.len();
        state.pending_image_attachments.clear();
        println!("  {GREEN}✓{RESET} {DIM}cleared {n} staged image(s){RESET}");
        return;
    }
    let path = std::path::Path::new(args);
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            println!(
                "  {RED}✗{RESET} {DIM}cannot read {}: {e}{RESET}",
                path.display()
            );
            return;
        }
    };
    let mime = guess_image_mime(path);
    let data_url = format!(
        "data:{mime};base64,{}",
        crate::tools::image_base64_for_data_url(&bytes)
    );
    let supports_vision = catalog::lookup(state.config.backend, &state.model)
        .map(|m| m.vision)
        .unwrap_or(state.config.backend.is_local());
    if !supports_vision {
        println!(
            "  {YELLOW}!{RESET} {DIM}{} isn't catalog-marked as vision-capable — the model may reject the attachment.{RESET}",
            state.model
        );
    }
    state.pending_image_attachments.push(data_url);
    println!(
        "  {GREEN}✓{RESET} {DIM}image staged ({} bytes, {}). Send a prompt to attach.{RESET}",
        bytes.len(),
        mime
    );
}

fn cmd_reasoning(args: &str, state: &mut AppState) {
    let arg = args.trim().to_lowercase();
    let new_state = match arg.as_str() {
        "" | "status" => {
            let cur = state.renderer.reasoning_enabled();
            println!(
                "  {DIM}reasoning panel:{RESET} {} {DIM}(usage: /reasoning on|off){RESET}",
                if cur { "on" } else { "off" }
            );
            return;
        }
        "on" | "true" | "1" => true,
        "off" | "false" | "0" => false,
        other => {
            println!("  {RED}✗{RESET} {DIM}unknown value: {other} (use on, off, status){RESET}");
            return;
        }
    };
    state.renderer.set_reasoning(new_state);
    state.config.display.reasoning = new_state;
    println!(
        "  {GREEN}✓{RESET} {DIM}reasoning panel →{RESET} {CYAN}{}{RESET}",
        if new_state { "on" } else { "off" }
    );
}

fn guess_image_mime(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
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
    if !chosen.is_local() && backend(chosen).api_key.is_empty() {
        let env_name = match chosen {
            BackendName::Openrouter => "OPENROUTER_API_KEY",
            BackendName::OpenAi => "OPENAI_API_KEY",
            _ => "API key",
        };
        println!("  {RED}✗{RESET} {DIM}{env_name} not set in environment.{RESET}");
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
    let name_width = shown.iter().map(|m| m.len()).max().unwrap_or(0);
    for (i, m) in shown.iter().enumerate() {
        match catalog::lookup(state.config.backend, m) {
            Some(info) => println!(
                "  {DIM}{:>2}){RESET} {:<width$}  {DIM}{}{RESET}",
                i + 1,
                m,
                catalog::format_cost_label(info),
                width = name_width
            ),
            None => println!("  {DIM}{:>2}){RESET} {}", i + 1, m),
        }
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
        default_model(&cloud_backend, None)
    };
    println!("  {YELLOW}⇆{RESET} {BOLD}cloud{RESET} {DIM}{cloud_model}{RESET}");
    println!();

    let messages = vec![
        ChatMessage::System {
            content: state.config.render_system_prompt(),
        },
        ChatMessage::User {
            content: user_text.to_string().into(),
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
    let base_system_prompt = render_system_prompt_with_memory(
        &state.config,
        &state.backend,
        &active_tool_names,
        &last_prompt,
    );
    let system_prompt =
        merge_system_prompt(&base_system_prompt, state.conversation_summary.as_deref());
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
        state.conversation_summary.as_deref(),
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
    let base_system_prompt = render_system_prompt_with_memory(
        &state.config,
        &state.backend,
        &active_tool_names,
        &last_prompt,
    );
    let tools = build_tools_for_names(&state.config, &active_tool_names);
    let tool_defs = to_openai_tools(&tools);

    println!("  {DIM}Compacting older messages…{RESET}");
    let mut compact_ctx = CompactSessionContext {
        messages: &mut state.messages,
        system_prompt: &base_system_prompt,
        tool_defs: &tool_defs,
        config: &state.config,
        model: &state.model,
        is_local: state.backend.is_local,
        http: &state.http,
        backend: &state.backend,
        conversation_summary: state.conversation_summary.as_deref(),
    };
    let result = compact_session(
        &mut compact_ctx,
        &state.session_dir,
        &mut state.session_path,
        keep,
    )
    .await?;

    if !result.compacted {
        println!("  {DIM}Nothing to compact yet.{RESET}");
        return Ok(());
    }

    if let Some(summary) = result.conversation_summary {
        state.conversation_summary = Some(summary);
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
    println!("  {DIM}session → {}{RESET}", state.session_path.display());
    Ok(())
}

fn last_user_prompt(state: &AppState) -> Option<String> {
    state
        .messages
        .iter()
        .rev()
        .find_map(|m| m.user_text().map(|s| s.into_owned()))
}

fn cmd_undo(args: &str, state: &mut AppState) -> Result<()> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.first() == Some(&"list") {
        if state.checkpoint_stack.is_empty() {
            println!("  {DIM}No checkpoints to undo.{RESET}");
            return Ok(());
        }
        println!(
            "  {DIM}Checkpoint stack ({}){RESET}",
            state.checkpoint_stack.len()
        );
        for (idx, cp) in state.checkpoint_stack.checkpoints.iter().rev().enumerate() {
            println!(
                "  {CYAN}{idx}.{RESET} {DIM}{}{RESET} · {} file(s) · skipped {}",
                cp.created_at,
                cp.file_count(),
                cp.skipped.len()
            );
        }
        return Ok(());
    }

    let Some(checkpoint) = state.checkpoint_stack.pop() else {
        println!("  {DIM}Nothing to undo — no checkpoint from a prior mutating turn.{RESET}");
        return Ok(());
    };

    let workspace = Path::new(&state.config.workspace_root);
    let report = crate::turn_checkpoint::restore_checkpoint(&checkpoint, workspace);
    println!("  {GREEN}✓{RESET} {DIM}undo {}{RESET}", checkpoint.id);
    if !report.restored.is_empty() {
        println!(
            "  {DIM}restored {} file(s): {}{RESET}",
            report.restored.len(),
            report.restored.join(", ")
        );
    }
    if !report.removed.is_empty() {
        println!(
            "  {DIM}removed {} created file(s): {}{RESET}",
            report.removed.len(),
            report.removed.join(", ")
        );
    }
    if report.is_partial() {
        println!("  {YELLOW}!{RESET} {DIM}partial undo — some paths were skipped or failed{RESET}");
        if !report.skipped.is_empty() {
            println!("  {DIM}skipped: {}{RESET}", report.skipped.join(", "));
        }
        for err in &report.errors {
            println!("  {RED}✗{RESET} {DIM}{err}{RESET}");
        }
    }
    Ok(())
}

fn cmd_checkpoints(args: &str, state: &mut AppState) {
    let arg = args.trim();
    match arg {
        "on" => {
            state.checkpoints_enabled = true;
            println!("  {GREEN}✓{RESET} {DIM}turn checkpoints enabled for this session{RESET}");
        }
        "off" => {
            state.checkpoints_enabled = false;
            println!("  {GREEN}✓{RESET} {DIM}turn checkpoints disabled for this session{RESET}");
        }
        "status" | "" => {
            println!(
                "  {DIM}checkpoints{RESET}  config={} session={} stack={}/{} maxFileBytes={}",
                state.config.checkpoints.enabled,
                state.checkpoints_enabled,
                state.checkpoint_stack.len(),
                state.checkpoint_stack.limits.max_turns,
                state.config.checkpoints.max_file_bytes
            );
        }
        other => {
            println!("  {DIM}Usage: /checkpoints [on|off|status]{RESET} (unknown: {other})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::BackendDescriptor;
    use crate::config::AgentConfig;
    use crate::session_paths::PathStore;
    use std::process::Command;

    fn test_state(root: &Path) -> AppState {
        let mut config = AgentConfig {
            workspace_root: root.display().to_string(),
            session_dir: root.join(".sessions").display().to_string(),
            ..Default::default()
        };
        config.project_memory.max_injected_bytes = 1024;
        config.paths.enabled = true;
        let session_path = root.join(".sessions/test.jsonl");
        AppState {
            http: reqwest::Client::new(),
            backend: backend(config.backend),
            model: "test-model".into(),
            messages: Vec::new(),
            session_dir: config.session_dir.clone(),
            session_path,
            total_in: 0,
            total_out: 0,
            session_usd: 0.0,
            session_cost_has_unknown: false,
            context_guard_notice: None,
            conversation_summary: None,
            checkpoint_stack: crate::turn_checkpoint::CheckpointStack::new(
                config.checkpoints.limits(),
            ),
            checkpoints_enabled: config.checkpoints.enabled,
            play_session: None,
            last_play_scorecard: None,
            approval_cache: crate::approval::ApprovalCache::new(),
            renderer: crate::renderer::TuiRenderer::new(config.display.clone()),
            warmed_fingerprint: None,
            tests_ran_this_session: false,
            pending_image_attachments: Vec::new(),
            mcp_tools: Vec::new(),
            path_store: PathStore::new(
                &config.session_dir,
                &root.join(".sessions/test.jsonl"),
                &config.paths,
            ),
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

    #[test]
    fn undo_restores_mutated_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("note.txt"), "before\n").unwrap();
        let mut state = test_state(dir.path());
        let mut checkpoint = crate::turn_checkpoint::TurnCheckpoint::new();
        crate::turn_checkpoint::snapshot_file_into(
            &mut checkpoint,
            dir.path(),
            "note.txt",
            state.config.checkpoints.limits(),
        )
        .unwrap();
        fs::write(dir.path().join("note.txt"), "after\n").unwrap();
        state.checkpoint_stack.push(checkpoint);

        cmd_undo("", &mut state).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "before\n"
        );
        assert!(state.checkpoint_stack.is_empty());
    }

    #[test]
    fn new_restores_play_session_and_clears_session_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path());
        let original_root = state.config.workspace_root.clone();
        let original_mode = state.config.mode;
        let sandbox = dir.path().join("sandbox");
        state.play_session = Some(crate::app_state::PlaySession {
            fixture_id: "demo".into(),
            sandbox_root: sandbox.clone(),
            restore: crate::app_state::PlayRestoreSnapshot {
                config: state.config.clone(),
                checkpoints_enabled: true,
            },
        });
        state.config.workspace_root = sandbox.display().to_string();
        state.config.apply_operator_mode(OperatorMode::Ship);
        state.messages.push(crate::openai::ChatMessage::User {
            content: "hello".into(),
        });
        state.conversation_summary = Some("summary".into());
        state.context_guard_notice = Some("notice".into());
        state
            .checkpoint_stack
            .push(crate::turn_checkpoint::TurnCheckpoint::new());

        cmd_new(&mut state);

        assert_eq!(state.config.workspace_root, original_root);
        assert_eq!(state.config.mode, original_mode);
        assert!(state.play_session.is_none());
        assert!(state.messages.is_empty());
        assert!(state.conversation_summary.is_none());
        assert!(state.context_guard_notice.is_none());
        assert!(state.checkpoint_stack.is_empty());
    }

    #[test]
    fn mode_ship_syncs_session_checkpoints_flag() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path());
        state.config.apply_operator_mode(OperatorMode::Explore);
        state.checkpoints_enabled = state.config.checkpoints.enabled;
        assert!(!state.checkpoints_enabled);

        cmd_mode("ship", &mut state);

        assert_eq!(state.config.mode, OperatorMode::Ship);
        assert!(state.config.checkpoints.enabled);
        assert!(state.checkpoints_enabled);
    }

    #[test]
    fn path_fork_refuses_during_play_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path());
        state.play_session = Some(crate::app_state::PlaySession {
            fixture_id: "demo".into(),
            sandbox_root: dir.path().join("sandbox"),
            restore: crate::app_state::PlayRestoreSnapshot {
                config: state.config.clone(),
                checkpoints_enabled: true,
            },
        });
        let err = ensure_path_ops_allowed(&state).unwrap_err();
        assert!(err.to_string().contains("/play"));
    }

    #[test]
    fn new_resets_path_store_for_fresh_session() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("alpha.txt"), "main\n").unwrap();
        let mut state = test_state(dir.path());
        let root = state.workspace_root();
        let session_path = state.session_path.clone();
        let current = PathStore::capture_state(&state, &root).unwrap();
        state
            .path_store
            .fork(current, &session_path, Some("plan-a"), &root)
            .unwrap();
        assert!(state.path_store.path_count() >= 2);

        cmd_new(&mut state);

        assert_eq!(state.path_store.active_id(), DEFAULT_PATH_ID);
        assert_eq!(state.path_store.path_count(), 1);
        assert!(state.path_store.registry.paths.is_empty());
    }
}

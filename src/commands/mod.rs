use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use crate::agent::to_openai_tools;
use crate::agent_eval::{builtin_fixtures, render_agent_eval_markdown, run_agent_eval};
use crate::app_state::AppState;
use crate::auto_loop::{parse_auto_args, parse_done_criteria, run_auto_loop, run_done_check};
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
use crate::continuation::{
    continuation_system_prompt, default_continuation_path, ensure_continuation_sections,
    render_continuation_prompt, render_fallback_continuation,
};
use crate::fix_loop::{parse_fix_args, run_fix_loop};
use crate::handoff::{
    collect_handoff_context, default_export_path as default_handoff_export_path,
    ensure_required_sections, handoff_system_prompt, render_fallback_markdown,
    render_handoff_prompt, should_refuse_cloud_handoff, HandoffBasis, HandoffContext,
    HANDOFF_CONTEXT_LIMIT_BYTES,
};
use crate::hardware::{detect_hardware_spec, save_hardware_summary, HardwareSpec};
use crate::input::plain_read_line;
use crate::iterate_loop::{collect_diff_context, parse_iterate_args, run_iterate_loop};
use crate::openai::{
    list_models, stream_chat, ChatMessage, ChatRequest, StreamOptions, ToolDef, ToolDefFunction,
};
use crate::planner::{
    default_spec_path, ensure_spec_sections, planner_system_prompt, render_fallback_spec,
    render_planner_prompt,
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
mod config_cmds;
mod context_cmds;
mod doctor;
mod memory;
mod session;
mod workflow;

pub(crate) use context_cmds::perform_reset;

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
        "/ship",
        "Preview final ship readiness and draft a commit message",
    ),
    (
        "/handoff",
        "Draft commit, changelog, testing, and X-ready release copy",
    ),
    (
        "/plan",
        "Expand a short intent into a product spec; /plan validate checks its Done Criteria against the diff",
    ),
    ("/session", "Show session info and token usage"),
    ("/sessions", "List saved sessions"),
    ("/resume", "Resume latest or named session"),
    ("/export", "Export a session to markdown or json"),
    (
        "/auth",
        "Manage API keys and OAuth credentials (list, set, clear, login)",
    ),
    (
        "/login",
        "Browser/device-code login for subscription providers (openai-codex)",
    ),
    ("/logout", "Clear an OAuth login (openai-codex)"),
    (
        "/image",
        "Attach an image to the next user prompt (vision-capable models only)",
    ),
    (
        "/reasoning",
        "Toggle the streaming reasoning panel (on, off, status)",
    ),
    (
        "/verbose",
        "Show every tool call with full args + result (on, off, status)",
    ),
    (
        "/trace",
        "Show nested subagent/critic tool activity (on, off, status)",
    ),
    (
        "/backend",
        "Switch backend (ollama, lm-studio, mlx, llamacpp, openrouter, openai, openai-codex)",
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
        "/reset",
        "Reset context: write a continuation handoff (.small-harness/continue.md) and start fresh",
    ),
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
    (
        "/iterate",
        "Generate→evaluate→improve loop on a goal (rubric-scored, --max, --threshold, --yolo)",
    ),
    (
        "/auto",
        "Autonomous overnight run: chains /iterate + auto /reset toward a goal/spec with budget/deadline guardrails",
    ),
];

pub async fn dispatch(input: &str, state: &mut AppState) -> Result<()> {
    let mut parts = input.splitn(2, ' ');
    let name = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim().to_string();

    match name {
        "/help" => help(),
        "/setup" => cmd_setup(state).await?,
        "/new" => session::cmd_new(state),
        "/clear" => clear_screen(),
        "/undo" => session::cmd_undo(&args, state)?,
        "/checkpoints" => context_cmds::cmd_checkpoints(&args, state),
        "/path" => session::cmd_path(&args, state).await?,
        "/paths" => session::cmd_paths(state)?,
        "/config" => config_cmds::cmd_config(state),
        "/mode" => config_cmds::cmd_mode(&args, state),
        "/shipcheck" => cmd_shipcheck(&args, state)?,
        "/ship" => cmd_ship(&args, state).await?,
        "/handoff" => cmd_handoff(&args, state).await?,
        "/plan" => cmd_plan(&args, state).await?,
        "/session" => session::cmd_session(&args, state)?,
        "/sessions" => session::cmd_sessions(&args, state)?,
        "/resume" => session::cmd_resume(&args, state)?,
        "/export" => session::cmd_export(&args, state)?,
        "/auth" => config_cmds::cmd_auth(&args).await?,
        "/login" => config_cmds::cmd_login(&args, state).await?,
        "/logout" => config_cmds::cmd_logout(&args)?,
        "/image" => config_cmds::cmd_image(&args, state),
        "/reasoning" => config_cmds::cmd_reasoning(&args, state),
        "/verbose" => config_cmds::cmd_verbose(&args, state),
        "/trace" => config_cmds::cmd_trace(&args, state),
        "/backend" => config_cmds::cmd_backend(&args, state).await?,
        "/model" => config_cmds::cmd_model(&args, state).await?,
        "/tools" => config_cmds::cmd_tools(&args, state),
        "/compare" => config_cmds::cmd_compare(&args, state).await?,
        "/context" => context_cmds::cmd_context(&args, state),
        "/compact" => context_cmds::cmd_compact(&args, state).await?,
        "/reset" => context_cmds::cmd_reset(&args, state).await?,
        "/doctor" => cmd_doctor(&args, state).await?,
        "/index" => memory::cmd_index(&args, state)?,
        "/map" => memory::cmd_map(&args, state)?,
        "/memory" => memory::cmd_memory(&args, state),
        "/remember" => memory::cmd_remember(&args, state)?,
        "/forget" => memory::cmd_forget(&args, state)?,
        "/eval" => cmd_eval(&args, state).await?,
        "/batch" => cmd_batch(&args, state)?,
        "/refactor" => cmd_refactor(&args, state)?,
        "/test" => cmd_test(&args, state)?,
        "/prompt" => cmd_prompt(&args, state).await?,
        "/play" => cmd_play(&args, state).await?,
        "/fix" => cmd_fix(&args, state).await?,
        "/iterate" => cmd_iterate(&args, state).await?,
        "/auto" => cmd_auto(&args, state).await?,
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

/// All slash commands (name + description) offered in the completion menu,
/// sorted by name for a stable order. Includes `/exit` and `/quit`, which are
/// handled in the input loop rather than via `COMMANDS`.
pub fn command_list() -> Vec<(String, String)> {
    let mut cmds: Vec<(String, String)> = COMMANDS
        .iter()
        .map(|(n, d)| ((*n).to_string(), (*d).to_string()))
        .collect();
    cmds.push(("/exit".to_string(), "Quit Small Harness".to_string()));
    cmds.push(("/quit".to_string(), "Quit (alias for /exit)".to_string()));
    cmds.sort_by(|a, b| a.0.cmp(&b.0));
    cmds.dedup_by(|a, b| a.0 == b.0);
    cmds
}

fn help() {
    for (n, d) in COMMANDS {
        println!("  {CYAN}{:<12}{RESET} {DIM}{}{RESET}", n, d);
    }
    println!(
        "  {CYAN}{:<12}{RESET} {DIM}Quit Small Harness{RESET}",
        "/exit"
    );
    println!(
        "  {CYAN}{:<12}{RESET} {DIM}Quit (alias for /exit){RESET}",
        "/quit"
    );
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

fn clear_screen() {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[2J\x1b[H");
    let _ = out.flush();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShipAction {
    Preview,
    Commit,
    Push,
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShipStaging {
    All,
    StagedOnly,
}

impl ShipStaging {
    fn label(self) -> &'static str {
        match self {
            ShipStaging::All => "all changes",
            ShipStaging::StagedOnly => "staged changes only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShipArgs {
    action: ShipAction,
    run_tests: Option<bool>,
    allow_behind: bool,
    allow_cloud: bool,
    staging: Option<ShipStaging>,
    yes: bool,
    message: Option<String>,
    pr_base: Option<String>,
    pr_ready: bool,
    pr_title: Option<String>,
}

fn parse_ship_args(args: &str) -> Option<ShipArgs> {
    let mut parsed = ShipArgs {
        action: ShipAction::Preview,
        run_tests: None,
        allow_behind: false,
        allow_cloud: false,
        staging: None,
        yes: false,
        message: None,
        pr_base: None,
        pr_ready: false,
        pr_title: None,
    };
    let mut action_seen = false;
    let mut parts = args.split_whitespace().peekable();

    while let Some(part) = parts.next() {
        match part {
            "" | "preview" | "--dry-run" => {}
            "commit" => {
                if action_seen {
                    return None;
                }
                parsed.action = ShipAction::Commit;
                action_seen = true;
            }
            "push" => {
                if action_seen {
                    return None;
                }
                parsed.action = ShipAction::Push;
                action_seen = true;
            }
            "pr" => {
                if action_seen {
                    return None;
                }
                parsed.action = ShipAction::Pr;
                action_seen = true;
            }
            "--tests" => parsed.run_tests = Some(true),
            "--no-tests" => parsed.run_tests = Some(false),
            "--allow-behind" => parsed.allow_behind = true,
            "--cloud" => parsed.allow_cloud = true,
            "--ready" => parsed.pr_ready = true,
            "--draft" => parsed.pr_ready = false,
            "--base" => {
                let value = parts.next()?;
                if value.trim().is_empty() {
                    return None;
                }
                parsed.pr_base = Some(value.to_string());
            }
            p if p.starts_with("--base=") => {
                let value = p.strip_prefix("--base=")?;
                if value.trim().is_empty() {
                    return None;
                }
                parsed.pr_base = Some(value.to_string());
            }
            "--all" => {
                if parsed.staging.replace(ShipStaging::All).is_some() {
                    return None;
                }
            }
            "--staged-only" => {
                if parsed.staging.replace(ShipStaging::StagedOnly).is_some() {
                    return None;
                }
            }
            "--yes" | "-y" => parsed.yes = true,
            "--message" | "-m" => {
                let rest = parts.collect::<Vec<_>>().join(" ");
                if rest.trim().is_empty() {
                    return None;
                }
                parsed.message = Some(rest);
                break;
            }
            "--title" => {
                let rest = parts.collect::<Vec<_>>().join(" ");
                if rest.trim().is_empty() {
                    return None;
                }
                parsed.pr_title = Some(rest);
                break;
            }
            p if p.starts_with("--title=") => {
                let value = p.strip_prefix("--title=")?;
                if value.trim().is_empty() {
                    return None;
                }
                let rest = parts.collect::<Vec<_>>().join(" ");
                parsed.pr_title = Some(if rest.trim().is_empty() {
                    value.to_string()
                } else {
                    format!("{value} {rest}")
                });
                break;
            }
            _ => return None,
        }
    }

    Some(parsed)
}

async fn cmd_ship(args: &str, state: &AppState) -> Result<()> {
    let Some(args) = parse_ship_args(args) else {
        println!(
            "  {DIM}Usage: /ship [preview|commit|push|pr] [--tests|--no-tests] [--all|--staged-only] [--allow-behind] [--cloud] [--yes] [-m <message>] [--base <branch>] [--ready] [--title <title>]{RESET}"
        );
        return Ok(());
    };

    if args.action != ShipAction::Commit && args.staging.is_some() {
        println!("  {DIM}Usage: staging flags only apply to /ship commit.{RESET}");
        return Ok(());
    }
    if matches!(args.action, ShipAction::Push | ShipAction::Pr) && args.message.is_some() {
        println!(
            "  {DIM}Usage: commit messages only apply to /ship preview or /ship commit.{RESET}"
        );
        return Ok(());
    }
    if args.action != ShipAction::Pr
        && (args.pr_base.is_some() || args.pr_title.is_some() || args.pr_ready)
    {
        println!("  {DIM}Usage: PR flags only apply to /ship pr.{RESET}");
        return Ok(());
    }
    if args.action == ShipAction::Commit && args.staging.is_none() {
        println!(
            "  {DIM}Usage: /ship commit --all | /ship commit --staged-only [--tests|--no-tests] [--yes] [-m <message>]{RESET}"
        );
        return Ok(());
    }

    let run_tests = args
        .run_tests
        .unwrap_or(matches!(args.action, ShipAction::Commit));
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
    let readiness = if args.action == ShipAction::Push {
        evaluate_ship_push_readiness(&snapshot, args.allow_behind)
    } else if args.action == ShipAction::Pr {
        evaluate_ship_pr_readiness(&snapshot, args.allow_behind)
    } else {
        evaluate_ship_readiness(&snapshot, args.allow_behind)
    };

    println!(
        "  {DIM}ship{RESET}            {}",
        match args.action {
            ShipAction::Preview => "preview",
            ShipAction::Commit => "commit",
            ShipAction::Push => "push",
            ShipAction::Pr => "pr",
        }
    );
    print_shipcheck(&snapshot, freshness.as_ref());
    print_ship_readiness(&readiness);

    if args.action == ShipAction::Preview {
        match collect_handoff_context(&snapshot)? {
            Some(context) => {
                let draft = draft_ship_commit_message(
                    &snapshot,
                    &context,
                    freshness.as_ref(),
                    state,
                    args.allow_cloud,
                    args.message.as_deref(),
                )
                .await;
                print_ship_commit_draft(&draft);
            }
            None => println!("  {DIM}commitDraft{RESET}     none (no changes or ahead commits)"),
        }
        return Ok(());
    }

    match args.action {
        ShipAction::Commit => {
            run_ship_commit(args, snapshot, freshness.as_ref(), readiness, state).await
        }
        ShipAction::Push => run_ship_push(args, snapshot, readiness, state).await,
        ShipAction::Pr => run_ship_pr(args, snapshot, readiness, state).await,
        ShipAction::Preview => Ok(()),
    }
}

async fn run_ship_commit(
    args: ShipArgs,
    snapshot: ShipcheckSnapshot,
    freshness: Option<&crate::project_memory::ProjectIndexFreshness>,
    readiness: ShipReadiness,
    state: &AppState,
) -> Result<()> {
    let staging = args.staging.expect("commit action requires staging mode");
    let commit_blockers = commit_specific_blockers(&snapshot, staging);
    for blocker in &commit_blockers {
        println!("  {RED}✗{RESET} {DIM}{blocker}{RESET}");
    }
    if readiness.status == ShipReadinessStatus::Blocked || !commit_blockers.is_empty() {
        println!("  {RED}✗{RESET} {DIM}commit not created{RESET}");
        return Ok(());
    }

    if staging == ShipStaging::All {
        let prompt = format!(
            "  {YELLOW}?{RESET} {DIM}Stage all working-tree changes with git add -A? [y/N]{RESET}"
        );
        if !args.yes && !confirm_ship_action(prompt).await? {
            println!("  {RED}✗{RESET} {DIM}ship commit cancelled before staging{RESET}");
            return Ok(());
        }
        run_git_capture(&state.config.workspace_root, &["add", "-A"])?;
    }

    let staged_snapshot = collect_shipcheck(&state.config.workspace_root)?;
    if staged_snapshot.staged_count() == 0 {
        println!("  {RED}✗{RESET} {DIM}commit not created: no staged changes{RESET}");
        return Ok(());
    }
    println!(
        "  {DIM}staging{RESET}         {} · {} staged file(s)",
        staging.label(),
        staged_snapshot.staged_count()
    );
    print_diff_stat("finalStagedDiff", &staged_snapshot.staged_diff_stat);

    let Some(context) = build_staged_ship_context(&staged_snapshot)? else {
        println!("  {RED}✗{RESET} {DIM}commit not created: staged diff is empty{RESET}");
        return Ok(());
    };
    let draft = draft_ship_commit_message(
        &staged_snapshot,
        &context,
        freshness,
        state,
        args.allow_cloud,
        args.message.as_deref(),
    )
    .await;
    print_ship_commit_draft(&draft);

    let prompt =
        format!("  {YELLOW}?{RESET} {DIM}Create git commit with this message? [y/N]{RESET}");
    if !args.yes && !confirm_ship_action(prompt).await? {
        println!("  {RED}✗{RESET} {DIM}ship commit cancelled before commit{RESET}");
        return Ok(());
    }

    let commit_hash = create_git_commit(&state.config.workspace_root, &draft.message)?;
    let record_path = write_ship_commit_record(
        &state.session_dir,
        &staged_snapshot,
        snapshot.test_status.as_ref(),
        staging,
        &draft.message,
        &commit_hash,
    )?;
    println!("  {GREEN}✓{RESET} {DIM}commit created:{RESET} {CYAN}{commit_hash}{RESET}");
    println!(
        "  {GREEN}✓{RESET} {DIM}ship record saved →{RESET} {}",
        record_path.display()
    );
    Ok(())
}

async fn run_ship_push(
    args: ShipArgs,
    snapshot: ShipcheckSnapshot,
    readiness: ShipReadiness,
    state: &AppState,
) -> Result<()> {
    if readiness.status == ShipReadinessStatus::Blocked {
        println!("  {RED}✗{RESET} {DIM}push not run{RESET}");
        return Ok(());
    }

    let target = match resolve_ship_push_target(&state.config.workspace_root, &snapshot) {
        Ok(target) => target,
        Err(e) => {
            println!("  {RED}✗{RESET} {DIM}push not run: {e}{RESET}");
            return Ok(());
        }
    };
    println!("  {DIM}pushTarget{RESET}      {}", target.description());

    let prompt = format!(
        "  {YELLOW}?{RESET} {DIM}Push {}? [y/N]{RESET}",
        target.description()
    );
    if !args.yes && !confirm_ship_action(prompt).await? {
        println!("  {RED}✗{RESET} {DIM}ship push cancelled{RESET}");
        return Ok(());
    }

    let output = execute_ship_push(&state.config.workspace_root, &target)?;
    let pushed_snapshot = collect_shipcheck(&state.config.workspace_root)?;
    let commit_hash = run_git_capture(
        &state.config.workspace_root,
        &["rev-parse", "--short", "HEAD"],
    )?
    .trim()
    .to_string();
    let record_path = write_ship_push_record(
        &state.session_dir,
        &pushed_snapshot,
        &target,
        &commit_hash,
        &output,
    )?;
    println!(
        "  {GREEN}✓{RESET} {DIM}pushed:{RESET} {CYAN}{}{RESET}",
        target.description()
    );
    println!(
        "  {GREEN}✓{RESET} {DIM}ship record saved →{RESET} {}",
        record_path.display()
    );
    Ok(())
}

async fn run_ship_pr(
    args: ShipArgs,
    snapshot: ShipcheckSnapshot,
    readiness: ShipReadiness,
    state: &AppState,
) -> Result<()> {
    if readiness.status == ShipReadinessStatus::Blocked {
        println!("  {RED}✗{RESET} {DIM}PR not created{RESET}");
        return Ok(());
    }

    let target = match resolve_ship_pr_target(
        &state.config.workspace_root,
        &snapshot,
        args.pr_base.as_deref(),
    ) {
        Ok(target) => target,
        Err(e) => {
            println!("  {RED}✗{RESET} {DIM}PR not created: {e}{RESET}");
            return Ok(());
        }
    };
    let commit_hash = run_git_capture(
        &state.config.workspace_root,
        &["rev-parse", "--short", "HEAD"],
    )?
    .trim()
    .to_string();
    let title = args
        .pr_title
        .as_deref()
        .and_then(normalize_commit_message)
        .unwrap_or_else(|| latest_commit_subject(&state.config.workspace_root));
    let commits = pr_commit_summary(&state.config.workspace_root, &target);
    let body = render_ship_pr_body(&snapshot, &target, &commit_hash, &commits);
    let command = build_gh_pr_command(&target, &title, &body, !args.pr_ready);

    println!("  {DIM}prTarget{RESET}        {}", target.description());
    println!("  {DIM}prTitle{RESET}         {title}");
    println!(
        "  {DIM}prMode{RESET}          {}",
        if args.pr_ready { "ready" } else { "draft" }
    );

    if !gh_cli_ready(&state.config.workspace_root) {
        println!("  {YELLOW}!{RESET} {DIM}GitHub CLI is unavailable or unauthenticated; run this manually:{RESET}");
        println!("    {}", shell_command_display(&command));
        let record_path = write_ship_pr_record(
            &state.session_dir,
            &ShipPrRecord {
                snapshot: &snapshot,
                target: &target,
                title: &title,
                body: &body,
                command: &command,
                url: None,
                status: "manual command printed",
            },
        )?;
        println!(
            "  {GREEN}✓{RESET} {DIM}ship record saved →{RESET} {}",
            record_path.display()
        );
        return Ok(());
    }

    let prompt = format!(
        "  {YELLOW}?{RESET} {DIM}Create {} PR {}? [y/N]{RESET}",
        if args.pr_ready { "ready" } else { "draft" },
        target.description()
    );
    if !args.yes && !confirm_ship_action(prompt).await? {
        println!("  {RED}✗{RESET} {DIM}ship PR cancelled{RESET}");
        return Ok(());
    }

    let output = run_command_capture_combined(&state.config.workspace_root, &command)?;
    let url = extract_url(&output);
    let record_path = write_ship_pr_record(
        &state.session_dir,
        &ShipPrRecord {
            snapshot: &snapshot,
            target: &target,
            title: &title,
            body: &body,
            command: &command,
            url: url.as_deref(),
            status: "created",
        },
    )?;
    match &url {
        Some(url) => println!("  {GREEN}✓{RESET} {DIM}PR created:{RESET} {CYAN}{url}{RESET}"),
        None => println!("  {GREEN}✓{RESET} {DIM}PR created{RESET}"),
    }
    println!(
        "  {GREEN}✓{RESET} {DIM}ship record saved →{RESET} {}",
        record_path.display()
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShipPrTarget {
    remote: String,
    repo: String,
    head_branch: String,
    base_branch: String,
}

impl ShipPrTarget {
    fn description(&self) -> String {
        format!(
            "{}:{} -> {}:{}",
            self.repo, self.head_branch, self.repo, self.base_branch
        )
    }
}

fn resolve_ship_pr_target(
    workspace_root: &str,
    snapshot: &ShipcheckSnapshot,
    explicit_base: Option<&str>,
) -> Result<ShipPrTarget> {
    let upstream = snapshot
        .branch
        .upstream
        .as_deref()
        .ok_or_else(|| anyhow!("branch has no upstream; run /ship push first"))?;
    let (remote, remote_branch) = upstream
        .split_once('/')
        .ok_or_else(|| anyhow!("upstream `{upstream}` does not include a remote"))?;
    let remote_url = run_git_capture(workspace_root, &["remote", "get-url", remote])?;
    let repo = parse_github_repo(remote_url.trim())
        .ok_or_else(|| anyhow!("remote `{remote}` is not a GitHub URL"))?;
    let base_branch = explicit_base
        .and_then(normalize_commit_message)
        .unwrap_or_else(|| {
            discover_default_branch(workspace_root, remote).unwrap_or_else(|| "main".into())
        });

    Ok(ShipPrTarget {
        remote: remote.to_string(),
        repo,
        head_branch: remote_branch.to_string(),
        base_branch,
    })
}

fn parse_github_repo(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches(".git");
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        return normalize_repo_slug(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        return normalize_repo_slug(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("ssh://git@github.com/") {
        return normalize_repo_slug(rest);
    }
    None
}

fn normalize_repo_slug(value: &str) -> Option<String> {
    let mut parts = value.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn discover_default_branch(workspace_root: &str, remote: &str) -> Option<String> {
    let output = run_git_capture(
        workspace_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ],
    )
    .ok()?;
    let trimmed = output.trim();
    trimmed
        .strip_prefix(&format!("{remote}/"))
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
}

fn latest_commit_subject(workspace_root: &str) -> String {
    run_git_capture(workspace_root, &["log", "-1", "--pretty=%s"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Ship Small Harness changes".into())
}

fn pr_commit_summary(workspace_root: &str, target: &ShipPrTarget) -> String {
    let base_ref = format!("{}/{}", target.remote, target.base_branch);
    let range = format!("{base_ref}..HEAD");
    run_git_capture(
        workspace_root,
        &["log", "--oneline", "--max-count=20", &range],
    )
    .ok()
    .filter(|s| !s.trim().is_empty())
    .or_else(|| run_git_capture(workspace_root, &["log", "--oneline", "--max-count=5"]).ok())
    .unwrap_or_else(|| "No commit summary available.".into())
}

fn render_ship_pr_body(
    snapshot: &ShipcheckSnapshot,
    target: &ShipPrTarget,
    commit_hash: &str,
    commits: &str,
) -> String {
    let mut out = String::new();
    out.push_str("## Summary\n\n");
    out.push_str("- Prepared by Small Harness `/ship pr`.\n");
    out.push_str(&format!("- Head: `{}`\n", target.head_branch));
    out.push_str(&format!("- Base: `{}`\n", target.base_branch));
    out.push_str(&format!("- Commit: `{commit_hash}`\n\n"));
    out.push_str("## Commits\n\n```text\n");
    out.push_str(commits.trim());
    out.push_str("\n```\n\n");
    out.push_str("## Ship Status\n\n");
    out.push_str(&format!("- Branch: `{}`\n", snapshot.branch_label()));
    out.push_str(&format!(
        "- Uncommitted files: {}\n",
        snapshot.staged_count() + snapshot.unstaged_count() + snapshot.untracked_count()
    ));
    out.push_str(&format!("- Conflicts: {}\n", snapshot.conflict_count()));
    out.push_str("\n## Tests\n\n");
    match &snapshot.test_status {
        Some(status) => {
            out.push_str(&format!(
                "- Framework: `{}`\n- Total: {}\n- Passed: {}\n- Failed: {}\n- Skipped: {}\n- Exit code: {}\n",
                status.framework,
                status.total,
                status.passed,
                status.failed,
                status.skipped,
                status.exit_code
            ));
            if let Some(error) = &status.error {
                out.push_str(&format!("- Error: `{error}`\n"));
            }
        }
        None => out.push_str("- Not run by `/ship pr`.\n"),
    }
    out
}

fn build_gh_pr_command(target: &ShipPrTarget, title: &str, body: &str, draft: bool) -> Vec<String> {
    let mut args = vec![
        "gh".to_string(),
        "pr".to_string(),
        "create".to_string(),
        "--repo".to_string(),
        target.repo.clone(),
        "--base".to_string(),
        target.base_branch.clone(),
        "--head".to_string(),
        target.head_branch.clone(),
        "--title".to_string(),
        title.to_string(),
        "--body".to_string(),
        body.to_string(),
    ];
    if draft {
        args.push("--draft".into());
    }
    args
}

fn gh_cli_ready(workspace_root: &str) -> bool {
    command_success(workspace_root, &["gh", "--version"])
        && command_success(workspace_root, &["gh", "auth", "status"])
}

fn command_success(workspace_root: &str, args: &[&str]) -> bool {
    let Some((program, rest)) = args.split_first() else {
        return false;
    };
    Command::new(program)
        .current_dir(workspace_root)
        .args(rest)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_command_capture_combined(workspace_root: &str, args: &[String]) -> Result<String> {
    let Some((program, rest)) = args.split_first() else {
        return Err(anyhow!("empty command"));
    };
    let output = Command::new(program)
        .current_dir(workspace_root)
        .args(rest)
        .output()
        .map_err(|e| anyhow!("failed to run {program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let detail = stderr.trim();
        return Err(anyhow!(
            "{} failed{}",
            shell_command_display(args),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let mut combined = String::new();
    combined.push_str(&stdout);
    if !stderr.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok(combined)
}

fn shell_command_display(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '='))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

fn extract_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|part| part.starts_with("https://") || part.starts_with("http://"))
        .map(|part| {
            part.trim_matches(|c: char| c == ')' || c == '(')
                .to_string()
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShipPushTarget {
    remote: String,
    local_branch: String,
    remote_branch: String,
    set_upstream: bool,
}

impl ShipPushTarget {
    fn description(&self) -> String {
        if self.set_upstream {
            format!(
                "{} -> {}/{} and set upstream",
                self.local_branch, self.remote, self.remote_branch
            )
        } else {
            format!(
                "{} -> {}/{}",
                self.local_branch, self.remote, self.remote_branch
            )
        }
    }
}

fn resolve_ship_push_target(
    workspace_root: &str,
    snapshot: &ShipcheckSnapshot,
) -> Result<ShipPushTarget> {
    let branch = snapshot
        .branch
        .head
        .as_deref()
        .filter(|head| !head.is_empty() && *head != "(detached)")
        .ok_or_else(|| anyhow!("current HEAD is detached or unknown"))?;

    if let Some(upstream) = snapshot.branch.upstream.as_deref() {
        let (remote, branch_name) = upstream
            .split_once('/')
            .ok_or_else(|| anyhow!("upstream `{upstream}` does not include a remote"))?;
        return Ok(ShipPushTarget {
            remote: remote.to_string(),
            local_branch: branch.to_string(),
            remote_branch: branch_name.to_string(),
            set_upstream: false,
        });
    }

    run_git_capture(workspace_root, &["remote", "get-url", "origin"])?;
    Ok(ShipPushTarget {
        remote: "origin".into(),
        local_branch: branch.to_string(),
        remote_branch: branch.to_string(),
        set_upstream: true,
    })
}

fn execute_ship_push(workspace_root: &str, target: &ShipPushTarget) -> Result<String> {
    if target.set_upstream {
        run_git_capture_combined(
            workspace_root,
            &["push", "-u", &target.remote, &target.local_branch],
        )
    } else {
        run_git_capture_combined(workspace_root, &["push"])
    }
}

fn commit_specific_blockers(snapshot: &ShipcheckSnapshot, staging: ShipStaging) -> Vec<String> {
    match staging {
        ShipStaging::All if snapshot.files.is_empty() => {
            vec!["no working-tree changes to stage for commit".into()]
        }
        ShipStaging::StagedOnly if snapshot.staged_count() == 0 => {
            vec!["no staged changes to commit; stage files first or use --all".into()]
        }
        _ => Vec::new(),
    }
}

async fn confirm_ship_action(prompt: String) -> Result<bool> {
    let answer = plain_read_line(format!("{prompt}\n  {YELLOW}? {RESET}")).await?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShipCommitDraft {
    message: String,
    note: Option<String>,
}

async fn draft_ship_commit_message(
    snapshot: &ShipcheckSnapshot,
    context: &crate::handoff::HandoffContext,
    freshness: Option<&crate::project_memory::ProjectIndexFreshness>,
    state: &AppState,
    allow_cloud: bool,
    explicit_message: Option<&str>,
) -> ShipCommitDraft {
    if let Some(message) = explicit_message.and_then(normalize_commit_message) {
        return ShipCommitDraft {
            message,
            note: Some("using explicit commit message".into()),
        };
    }

    if should_refuse_cloud_handoff(state.backend.name, allow_cloud) {
        let fallback = render_fallback_markdown(context, snapshot, freshness, None);
        return ShipCommitDraft {
            message: extract_markdown_section(&fallback, "Commit Message")
                .unwrap_or_else(|| fallback_commit_message(context)),
            note: Some(
                "cloud backend skipped for diff privacy; pass --cloud to draft with it".into(),
            ),
        };
    }

    let messages = vec![
        ChatMessage::System {
            content: handoff_system_prompt(),
        },
        ChatMessage::User {
            content: render_handoff_prompt(context, freshness).into(),
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
        max_tokens: Some(500),
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

    let (body, note) = match result {
        Ok(_) if !draft.trim().is_empty() => (ensure_required_sections(&draft), None),
        Ok(_) => (
            render_fallback_markdown(context, snapshot, freshness, Some("empty model response")),
            Some("model draft was empty; using deterministic fallback".into()),
        ),
        Err(e) => (
            render_fallback_markdown(context, snapshot, freshness, Some(&e.to_string())),
            Some(format!(
                "model draft failed; using deterministic fallback: {e}"
            )),
        ),
    };

    ShipCommitDraft {
        message: extract_markdown_section(&body, "Commit Message")
            .unwrap_or_else(|| fallback_commit_message(context)),
        note,
    }
}

fn fallback_commit_message(context: &crate::handoff::HandoffContext) -> String {
    match context.basis {
        crate::handoff::HandoffBasis::DirtyTree => {
            "feat: prepare local working tree handoff".into()
        }
        crate::handoff::HandoffBasis::AheadOfUpstream => "feat: summarize branch handoff".into(),
    }
}

fn normalize_commit_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn build_staged_ship_context(snapshot: &ShipcheckSnapshot) -> Result<Option<HandoffContext>> {
    if snapshot.staged_count() == 0 {
        return Ok(None);
    }
    let diff = run_git_capture(&snapshot.workspace_root, &["diff", "--cached", "--"])?;
    if diff.trim().is_empty() {
        return Ok(None);
    }

    let mut content = format!(
        "# Handoff Source Context\n\n## Basis\n\nStaged changes for commit\n\n## Shipcheck\n\n- Workspace: `{}`\n- Git root: `{}`\n- Branch: `{}`\n- Staged files: {}\n- Unstaged files: {}\n- Untracked files: {}\n- Conflicts: {}\n\n## Staged Diff\n\n```diff\n",
        snapshot.workspace_root,
        snapshot.git_root,
        snapshot.branch_label(),
        snapshot.staged_count(),
        snapshot.unstaged_count(),
        snapshot.untracked_count(),
        snapshot.conflict_count()
    );
    content.push_str(&truncate_for_ship_context(diff.trim()));
    content.push_str("\n```\n");

    Ok(Some(HandoffContext {
        basis: HandoffBasis::DirtyTree,
        content,
        truncated: diff.len() > HANDOFF_CONTEXT_LIMIT_BYTES,
    }))
}

fn truncate_for_ship_context(text: &str) -> String {
    if text.len() <= HANDOFF_CONTEXT_LIMIT_BYTES {
        return text.to_string();
    }
    let marker = "\n\n[... staged commit context truncated ...]";
    let max_prefix = HANDOFF_CONTEXT_LIMIT_BYTES.saturating_sub(marker.len());
    let mut end = max_prefix.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &text[..end], marker)
}

fn create_git_commit(workspace_root: &str, message: &str) -> Result<String> {
    let message = normalize_commit_message(message)
        .ok_or_else(|| anyhow!("commit message cannot be empty"))?;
    run_git_capture(workspace_root, &["commit", "-m", &message])?;
    Ok(
        run_git_capture(workspace_root, &["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string(),
    )
}

fn run_git_capture(workspace_root: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .map_err(|e| anyhow!("failed to run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(anyhow!(
            "git {} failed{}",
            args.join(" "),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_git_capture_combined(workspace_root: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .map_err(|e| anyhow!("failed to run git: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let detail = stderr.trim();
        return Err(anyhow!(
            "git {} failed{}",
            args.join(" "),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let mut combined = String::new();
    combined.push_str(&stdout);
    if !stderr.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok(combined)
}

fn default_ship_record_path(session_dir: &str) -> PathBuf {
    Path::new(session_dir).join("ship").join(format!(
        "{}.md",
        Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ")
    ))
}

fn write_ship_commit_record(
    session_dir: &str,
    staged_snapshot: &ShipcheckSnapshot,
    test_status: Option<&crate::shipcheck::TestStatus>,
    staging: ShipStaging,
    message: &str,
    commit_hash: &str,
) -> Result<PathBuf> {
    let path = default_ship_record_path(session_dir);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut out = String::new();
    out.push_str("# Small Harness Ship Commit\n\n");
    out.push_str(&format!("Generated: {}\n\n", Utc::now().to_rfc3339()));
    out.push_str("## Git\n\n");
    out.push_str(&format!("- Branch: `{}`\n", staged_snapshot.branch_label()));
    out.push_str(&format!("- Commit: `{commit_hash}`\n"));
    out.push_str(&format!("- Staging: `{}`\n", staging.label()));
    out.push_str(&format!(
        "- Staged files: {}\n- Unstaged files left: {}\n- Untracked files left: {}\n\n",
        staged_snapshot.staged_count(),
        staged_snapshot.unstaged_count(),
        staged_snapshot.untracked_count()
    ));
    out.push_str("## Commit Message\n\n```text\n");
    out.push_str(message.trim());
    out.push_str("\n```\n\n");
    out.push_str("## Tests\n\n");
    match test_status {
        Some(status) => {
            out.push_str(&format!(
                "- Framework: `{}`\n- Total: {}\n- Passed: {}\n- Failed: {}\n- Skipped: {}\n- Exit code: {}\n",
                status.framework,
                status.total,
                status.passed,
                status.failed,
                status.skipped,
                status.exit_code
            ));
            if let Some(error) = &status.error {
                out.push_str(&format!("- Error: `{error}`\n"));
            }
        }
        None => out.push_str("- Tests not run for this ship command.\n"),
    }
    out.push_str("\n## Final Staged Diff Stat\n\n");
    if staged_snapshot.staged_diff_stat.trim().is_empty() {
        out.push_str("No staged diff stat captured.\n");
    } else {
        out.push_str("```text\n");
        out.push_str(staged_snapshot.staged_diff_stat.trim());
        out.push_str("\n```\n");
    }

    fs::write(&path, out)?;
    Ok(path)
}

fn write_ship_push_record(
    session_dir: &str,
    snapshot: &ShipcheckSnapshot,
    target: &ShipPushTarget,
    commit_hash: &str,
    push_output: &str,
) -> Result<PathBuf> {
    let path = default_ship_record_path(session_dir);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut out = String::new();
    out.push_str("# Small Harness Ship Push\n\n");
    out.push_str(&format!("Generated: {}\n\n", Utc::now().to_rfc3339()));
    out.push_str("## Git\n\n");
    out.push_str(&format!("- Branch: `{}`\n", snapshot.branch_label()));
    out.push_str(&format!("- Commit: `{commit_hash}`\n"));
    out.push_str(&format!("- Remote: `{}`\n", target.remote));
    out.push_str(&format!("- Local branch: `{}`\n", target.local_branch));
    out.push_str(&format!("- Remote branch: `{}`\n", target.remote_branch));
    out.push_str(&format!("- Set upstream: `{}`\n", target.set_upstream));
    out.push_str(&format!(
        "- Uncommitted files left: {}\n- Untracked files left: {}\n\n",
        snapshot.staged_count() + snapshot.unstaged_count(),
        snapshot.untracked_count()
    ));
    out.push_str("## Push Output\n\n");
    if push_output.trim().is_empty() {
        out.push_str("No output captured.\n");
    } else {
        out.push_str("```text\n");
        out.push_str(push_output.trim());
        out.push_str("\n```\n");
    }

    fs::write(&path, out)?;
    Ok(path)
}

struct ShipPrRecord<'a> {
    snapshot: &'a ShipcheckSnapshot,
    target: &'a ShipPrTarget,
    title: &'a str,
    body: &'a str,
    command: &'a [String],
    url: Option<&'a str>,
    status: &'a str,
}

fn write_ship_pr_record(session_dir: &str, record: &ShipPrRecord<'_>) -> Result<PathBuf> {
    let path = default_ship_record_path(session_dir);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut out = String::new();
    out.push_str("# Small Harness Ship PR\n\n");
    out.push_str(&format!("Generated: {}\n\n", Utc::now().to_rfc3339()));
    out.push_str("## GitHub\n\n");
    out.push_str(&format!("- Status: `{}`\n", record.status));
    out.push_str(&format!("- Repository: `{}`\n", record.target.repo));
    out.push_str(&format!("- Head: `{}`\n", record.target.head_branch));
    out.push_str(&format!("- Base: `{}`\n", record.target.base_branch));
    if let Some(url) = record.url {
        out.push_str(&format!("- URL: {url}\n"));
    }
    out.push_str(&format!(
        "- Uncommitted files left: {}\n\n",
        record.snapshot.staged_count()
            + record.snapshot.unstaged_count()
            + record.snapshot.untracked_count()
    ));
    out.push_str("## Command\n\n```bash\n");
    out.push_str(&shell_command_display(record.command));
    out.push_str("\n```\n\n");
    out.push_str("## Title\n\n");
    out.push_str(record.title.trim());
    out.push_str("\n\n## Body\n\n");
    out.push_str(record.body.trim());
    out.push('\n');

    fs::write(&path, out)?;
    Ok(path)
}

fn extract_markdown_section(markdown: &str, section: &str) -> Option<String> {
    let mut in_section = false;
    let mut out = String::new();

    for line in markdown.lines() {
        if markdown_heading_text(line).is_some_and(|heading| heading.eq_ignore_ascii_case(section))
        {
            in_section = true;
            continue;
        }
        if in_section && markdown_heading_text(line).is_some() {
            break;
        }
        if in_section {
            out.push_str(line);
            out.push('\n');
        }
    }

    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn markdown_heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let text = trimmed.trim_start_matches('#').trim();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShipReadinessStatus {
    Ready,
    NeedsReview,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShipReadiness {
    status: ShipReadinessStatus,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

fn evaluate_ship_readiness(snapshot: &ShipcheckSnapshot, allow_behind: bool) -> ShipReadiness {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if snapshot.conflict_count() > 0 {
        blockers.push(format!(
            "{} conflicted file(s) must be resolved",
            snapshot.conflict_count()
        ));
    }
    if snapshot.branch.behind > 0 && !allow_behind {
        blockers.push(format!(
            "branch is behind upstream by {} commit(s); pull/rebase or pass --allow-behind",
            snapshot.branch.behind
        ));
    }
    if snapshot.is_clean() && snapshot.branch.ahead == 0 {
        blockers.push("nothing to ship: no working-tree changes and no ahead commits".into());
    }
    if let Some(tests) = &snapshot.test_status {
        if tests.failed > 0 || tests.exit_code != 0 {
            blockers.push(format!(
                "tests failed: {} failed, exit code {}",
                tests.failed, tests.exit_code
            ));
        }
        if let Some(error) = &tests.error {
            blockers.push(format!("test execution error: {error}"));
        }
    } else {
        warnings.push("tests were not run; use /ship --tests before committing".into());
    }
    if snapshot.branch.upstream.is_none() {
        warnings.push("no upstream configured; push/PR phases will need a remote target".into());
    }
    if snapshot.untracked_count() > 0 {
        warnings.push(format!(
            "{} untracked file(s) need an explicit staging decision",
            snapshot.untracked_count()
        ));
    }

    let status = if !blockers.is_empty() {
        ShipReadinessStatus::Blocked
    } else if !warnings.is_empty() {
        ShipReadinessStatus::NeedsReview
    } else {
        ShipReadinessStatus::Ready
    };

    ShipReadiness {
        status,
        blockers,
        warnings,
    }
}

fn evaluate_ship_push_readiness(snapshot: &ShipcheckSnapshot, allow_behind: bool) -> ShipReadiness {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if snapshot.branch.head.as_deref() == Some("(detached)") || snapshot.branch.head.is_none() {
        blockers.push("cannot push from a detached or unknown HEAD".into());
    }
    if snapshot.conflict_count() > 0 {
        blockers.push(format!(
            "{} conflicted file(s) must be resolved before push",
            snapshot.conflict_count()
        ));
    }
    if snapshot.branch.behind > 0 && !allow_behind {
        blockers.push(format!(
            "branch is behind upstream by {} commit(s); pull/rebase or pass --allow-behind",
            snapshot.branch.behind
        ));
    }
    if snapshot.branch.upstream.is_some() && snapshot.branch.ahead == 0 {
        blockers.push("nothing to push: branch is not ahead of upstream".into());
    }
    if snapshot.branch.upstream.is_none() {
        warnings.push("no upstream configured; /ship push will use origin and set upstream".into());
    }
    if snapshot.staged_count() + snapshot.unstaged_count() > 0 || snapshot.untracked_count() > 0 {
        warnings.push(format!(
            "{} uncommitted file(s) are not part of this push",
            snapshot.staged_count() + snapshot.unstaged_count() + snapshot.untracked_count()
        ));
    }
    if snapshot.test_status.is_none() {
        warnings.push("tests were not run for this push; use /ship push --tests if desired".into());
    }
    if let Some(tests) = &snapshot.test_status {
        if tests.failed > 0 || tests.exit_code != 0 {
            blockers.push(format!(
                "tests failed: {} failed, exit code {}",
                tests.failed, tests.exit_code
            ));
        }
        if let Some(error) = &tests.error {
            blockers.push(format!("test execution error: {error}"));
        }
    }

    let status = if !blockers.is_empty() {
        ShipReadinessStatus::Blocked
    } else if !warnings.is_empty() {
        ShipReadinessStatus::NeedsReview
    } else {
        ShipReadinessStatus::Ready
    };

    ShipReadiness {
        status,
        blockers,
        warnings,
    }
}

fn evaluate_ship_pr_readiness(snapshot: &ShipcheckSnapshot, allow_behind: bool) -> ShipReadiness {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if snapshot.branch.head.as_deref() == Some("(detached)") || snapshot.branch.head.is_none() {
        blockers.push("cannot create a PR from a detached or unknown HEAD".into());
    }
    if snapshot.branch.upstream.is_none() {
        blockers.push("branch has no upstream; run /ship push first".into());
    }
    if snapshot.branch.ahead > 0 {
        blockers.push(format!(
            "branch has {} unpushed commit(s); run /ship push first",
            snapshot.branch.ahead
        ));
    }
    if snapshot.branch.behind > 0 && !allow_behind {
        blockers.push(format!(
            "branch is behind upstream by {} commit(s); pull/rebase or pass --allow-behind",
            snapshot.branch.behind
        ));
    }
    if snapshot.conflict_count() > 0 {
        blockers.push(format!(
            "{} conflicted file(s) must be resolved before opening a PR",
            snapshot.conflict_count()
        ));
    }
    if snapshot.staged_count() + snapshot.unstaged_count() > 0 || snapshot.untracked_count() > 0 {
        warnings.push(format!(
            "{} uncommitted file(s) are not part of this PR",
            snapshot.staged_count() + snapshot.unstaged_count() + snapshot.untracked_count()
        ));
    }
    if snapshot.test_status.is_none() {
        warnings.push("tests were not run for this PR; use /ship pr --tests if desired".into());
    }
    if let Some(tests) = &snapshot.test_status {
        if tests.failed > 0 || tests.exit_code != 0 {
            blockers.push(format!(
                "tests failed: {} failed, exit code {}",
                tests.failed, tests.exit_code
            ));
        }
        if let Some(error) = &tests.error {
            blockers.push(format!("test execution error: {error}"));
        }
    }

    let status = if !blockers.is_empty() {
        ShipReadinessStatus::Blocked
    } else if !warnings.is_empty() {
        ShipReadinessStatus::NeedsReview
    } else {
        ShipReadinessStatus::Ready
    };

    ShipReadiness {
        status,
        blockers,
        warnings,
    }
}

fn print_ship_readiness(readiness: &ShipReadiness) {
    let (label, color) = match readiness.status {
        ShipReadinessStatus::Ready => ("ready", GREEN),
        ShipReadinessStatus::NeedsReview => ("needs review", YELLOW),
        ShipReadinessStatus::Blocked => ("blocked", RED),
    };
    println!("  {DIM}verdict{RESET}         {color}{label}{RESET}");
    for blocker in &readiness.blockers {
        println!("  {RED}✗{RESET} {DIM}{blocker}{RESET}");
    }
    for warning in &readiness.warnings {
        println!("  {YELLOW}!{RESET} {DIM}{warning}{RESET}");
    }
}

fn print_ship_commit_draft(draft: &ShipCommitDraft) {
    println!("  {DIM}commitDraft{RESET}");
    for line in draft.message.lines() {
        if line.trim().is_empty() {
            println!();
        } else {
            println!("    {line}");
        }
    }
    if let Some(note) = &draft.note {
        println!("  {DIM}{note}{RESET}");
    }
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

enum PlanInvocation {
    Show,
    Validate,
    Draft {
        intent: String,
        export_path: Option<PathBuf>,
    },
}

/// Parse `/plan` arguments. Returns `None` to print usage.
///   `/plan <intent>`                 → draft to `.small-harness/spec.md`
///   `/plan <intent> --export <path>` → draft to `<path>` instead
///   `/plan show`                     → print the saved spec
///   `/plan validate`                 → check the spec's Done Criteria vs the diff
fn parse_plan_args(args: &str) -> Option<PlanInvocation> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "show" {
        return Some(PlanInvocation::Show);
    }
    if trimmed == "validate" {
        return Some(PlanInvocation::Validate);
    }

    let mut export_path: Option<PathBuf> = None;
    let mut intent_parts: Vec<&str> = Vec::new();
    let mut parts = trimmed.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "--export" {
            export_path = Some(PathBuf::from(parts.next()?));
        } else if let Some(value) = part.strip_prefix("--export=") {
            if value.is_empty() {
                return None;
            }
            export_path = Some(PathBuf::from(value));
        } else {
            intent_parts.push(part);
        }
    }

    let intent = intent_parts.join(" ");
    if intent.trim().is_empty() {
        return None;
    }
    Some(PlanInvocation::Draft {
        intent,
        export_path,
    })
}

async fn cmd_plan(args: &str, state: &AppState) -> Result<()> {
    let Some(invocation) = parse_plan_args(args) else {
        println!(
            "  {DIM}Usage: /plan <intent>  ·  /plan <intent> --export <path>  ·  /plan show  ·  /plan validate{RESET}"
        );
        return Ok(());
    };

    let default_path = default_spec_path(&state.config.workspace_root);

    let (intent, export_path) = match invocation {
        PlanInvocation::Show => {
            match fs::read_to_string(&default_path) {
                Ok(content) => {
                    println!();
                    print!("{content}");
                }
                Err(_) => println!(
                    "  {DIM}No spec yet at {} — run /plan <intent> to create one.{RESET}",
                    default_path.display()
                ),
            }
            return Ok(());
        }
        PlanInvocation::Validate => return cmd_plan_validate(state, &default_path).await,
        PlanInvocation::Draft {
            intent,
            export_path,
        } => (intent, export_path),
    };

    println!(
        "  {DIM}expanding spec with {} · {}{RESET}",
        state.config.backend.as_str(),
        state.model
    );

    let messages = vec![
        ChatMessage::System {
            content: planner_system_prompt(),
        },
        ChatMessage::User {
            content: render_planner_prompt(&intent).into(),
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
        max_tokens: Some(1500),
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
        Ok(_) if !draft.trim().is_empty() => ensure_spec_sections(&draft),
        Ok(_) => render_fallback_spec(&intent, Some("empty model response")),
        Err(e) => render_fallback_spec(&intent, Some(&e.to_string())),
    };

    println!();
    print!("{body}");

    let out_path = export_path.unwrap_or(default_path);
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(&out_path, body)?;
    println!(
        "  {GREEN}✓{RESET} {DIM}spec saved →{RESET} {}",
        out_path.display()
    );

    Ok(())
}

/// `/plan validate`: read the saved spec's Done Criteria and check each one
/// against the current working-tree diff, printing a met/unmet checklist. The
/// same done-check `/auto` runs each round, exposed as a one-shot command so you
/// can ask "am I done?" by hand. Sends the diff to the model, so it honors the
/// same cloud-handoff refusal as `/iterate`.
async fn cmd_plan_validate(state: &AppState, spec_path: &Path) -> Result<()> {
    let spec = match fs::read_to_string(spec_path) {
        Ok(s) => s,
        Err(_) => {
            println!(
                "  {DIM}No spec yet at {} — run /plan <intent> to create one.{RESET}",
                spec_path.display()
            );
            return Ok(());
        }
    };
    let criteria = parse_done_criteria(&spec);
    if criteria.is_empty() {
        println!(
            "  {DIM}No Done Criteria found in {} — nothing to validate.{RESET}",
            spec_path.display()
        );
        return Ok(());
    }
    if should_refuse_cloud_handoff(state.backend.name, state.config.rubric.allow_cloud) {
        println!(
            "  {RED}✗{RESET} {DIM}/plan validate sends the working diff to the model — run on a local backend or set rubric.allowCloud.{RESET}"
        );
        return Ok(());
    }

    let model = state
        .config
        .iterate
        .evaluator_model
        .clone()
        .unwrap_or_else(|| state.model.clone());
    println!(
        "  {DIM}checking {} Done Criteria against the working tree with {}{RESET}",
        criteria.len(),
        model
    );
    let diff = collect_diff_context(&state.config.workspace_root);
    let check = run_done_check(state, &model, &criteria, &diff).await;
    println!();
    print!("{}", render_validate_report(&criteria, &check.met));
    Ok(())
}

/// Render the Done-Criteria checklist for `/plan validate`. Pure for testing.
fn render_validate_report(criteria: &[String], met: &[bool]) -> String {
    let mut out = String::new();
    for (i, c) in criteria.iter().enumerate() {
        let ok = met.get(i).copied().unwrap_or(false);
        let (mark, color) = if ok { ("✓", GREEN) } else { ("✗", RED) };
        out.push_str(&format!("  {color}{mark}{RESET} {c}\n"));
    }
    let met_count = met.iter().filter(|m| **m).count();
    out.push_str(&format!(
        "  {DIM}{}/{} criteria met{RESET}\n",
        met_count,
        criteria.len()
    ));
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::BackendDescriptor;
    use crate::config::AgentConfig;
    use crate::shipcheck::{GitBranchState, GitFileKind, GitFileState, TestStatus};
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn test_state(root: &Path) -> AppState {
        use crate::session_paths::PathStore;
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
            trace: crate::turn_trace::test_trace_for(&root.join(".sessions/test.jsonl")),
            trace_enabled: false,
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

    fn sample_snapshot(
        files: Vec<GitFileState>,
        test_status: Option<TestStatus>,
    ) -> ShipcheckSnapshot {
        ShipcheckSnapshot {
            workspace_root: "/tmp/workspace".into(),
            git_root: "/tmp/workspace".into(),
            branch: GitBranchState {
                head: Some("feature".into()),
                upstream: Some("origin/feature".into()),
                ..Default::default()
            },
            files,
            staged_diff_stat: String::new(),
            unstaged_diff_stat: String::new(),
            test_status,
        }
    }

    fn tracked_change(path: &str) -> GitFileState {
        GitFileState {
            path: path.into(),
            original_path: None,
            staged: Some('.'),
            unstaged: Some('M'),
            kind: GitFileKind::Tracked,
        }
    }

    fn passing_tests() -> TestStatus {
        TestStatus {
            framework: "cargo".into(),
            total: 3,
            passed: 3,
            failed: 0,
            skipped: 0,
            exit_code: 0,
            error: None,
        }
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

    #[test]
    fn parses_ship_preview_args() {
        let args = parse_ship_args("--tests --allow-behind --cloud").unwrap();
        assert_eq!(args.action, ShipAction::Preview);
        assert_eq!(args.run_tests, Some(true));
        assert!(args.allow_behind);
        assert!(args.allow_cloud);

        let args = parse_ship_args("preview --dry-run --no-tests").unwrap();
        assert_eq!(args.action, ShipAction::Preview);
        assert_eq!(args.run_tests, Some(false));

        assert_eq!(
            parse_ship_args("commit").unwrap().action,
            ShipAction::Commit
        );
        assert!(parse_ship_args("commit push").is_none());
        assert!(parse_ship_args("--unknown").is_none());
    }

    #[test]
    fn parses_ship_commit_staging_and_message_args() {
        let args = parse_ship_args("commit --all --yes -m feat: add ship commit").unwrap();
        assert_eq!(args.action, ShipAction::Commit);
        assert_eq!(args.staging, Some(ShipStaging::All));
        assert!(args.yes);
        assert_eq!(args.message.as_deref(), Some("feat: add ship commit"));

        let args = parse_ship_args("commit --staged-only --no-tests").unwrap();
        assert_eq!(args.staging, Some(ShipStaging::StagedOnly));
        assert_eq!(args.run_tests, Some(false));

        assert!(parse_ship_args("commit --all --staged-only").is_none());
        assert!(parse_ship_args("commit --message").is_none());
    }

    #[test]
    fn parses_ship_push_args() {
        let args = parse_ship_args("push --yes --allow-behind --tests").unwrap();
        assert_eq!(args.action, ShipAction::Push);
        assert!(args.yes);
        assert!(args.allow_behind);
        assert_eq!(args.run_tests, Some(true));

        assert!(parse_ship_args("push --all").is_some());
    }

    #[test]
    fn parses_ship_pr_args() {
        let args =
            parse_ship_args("pr --base main --ready --yes --title Add ship PR flow").unwrap();
        assert_eq!(args.action, ShipAction::Pr);
        assert_eq!(args.pr_base.as_deref(), Some("main"));
        assert!(args.pr_ready);
        assert!(args.yes);
        assert_eq!(args.pr_title.as_deref(), Some("Add ship PR flow"));

        let args = parse_ship_args("pr --base=develop --draft --title=Draft title").unwrap();
        assert_eq!(args.pr_base.as_deref(), Some("develop"));
        assert!(!args.pr_ready);
        assert_eq!(args.pr_title.as_deref(), Some("Draft title"));
    }

    #[test]
    fn extracts_commit_message_section() {
        let markdown =
            "## Commit Message\n\nfeat: ship preview\n\nBody line\n\n## Testing\n\ncargo test";
        assert_eq!(
            extract_markdown_section(markdown, "commit message").as_deref(),
            Some("feat: ship preview\n\nBody line")
        );
        assert!(extract_markdown_section(markdown, "missing").is_none());
    }

    #[test]
    fn ship_readiness_blocks_clean_tree_without_ahead_commits() {
        let snapshot = sample_snapshot(Vec::new(), Some(passing_tests()));
        let readiness = evaluate_ship_readiness(&snapshot, false);
        assert_eq!(readiness.status, ShipReadinessStatus::Blocked);
        assert!(readiness
            .blockers
            .iter()
            .any(|b| b.contains("nothing to ship")));
    }

    #[test]
    fn ship_readiness_ready_with_changes_and_passing_tests() {
        let snapshot = sample_snapshot(vec![tracked_change("src/main.rs")], Some(passing_tests()));
        let readiness = evaluate_ship_readiness(&snapshot, false);
        assert_eq!(readiness.status, ShipReadinessStatus::Ready);
        assert!(readiness.blockers.is_empty());
        assert!(readiness.warnings.is_empty());
    }

    #[test]
    fn ship_readiness_warns_when_tests_are_not_run() {
        let snapshot = sample_snapshot(vec![tracked_change("src/main.rs")], None);
        let readiness = evaluate_ship_readiness(&snapshot, false);
        assert_eq!(readiness.status, ShipReadinessStatus::NeedsReview);
        assert!(readiness
            .warnings
            .iter()
            .any(|w| w.contains("tests were not run")));
    }

    #[test]
    fn ship_readiness_blocks_behind_branch_unless_allowed() {
        let mut snapshot =
            sample_snapshot(vec![tracked_change("src/main.rs")], Some(passing_tests()));
        snapshot.branch.behind = 2;

        let blocked = evaluate_ship_readiness(&snapshot, false);
        assert_eq!(blocked.status, ShipReadinessStatus::Blocked);

        let allowed = evaluate_ship_readiness(&snapshot, true);
        assert_eq!(allowed.status, ShipReadinessStatus::Ready);
    }

    #[test]
    fn ship_push_readiness_blocks_when_not_ahead_of_upstream() {
        let snapshot = sample_snapshot(Vec::new(), None);
        let readiness = evaluate_ship_push_readiness(&snapshot, false);
        assert_eq!(readiness.status, ShipReadinessStatus::Blocked);
        assert!(readiness
            .blockers
            .iter()
            .any(|b| b.contains("nothing to push")));
    }

    #[test]
    fn ship_push_readiness_allows_new_upstream_with_warning() {
        let mut snapshot = sample_snapshot(Vec::new(), None);
        snapshot.branch.upstream = None;
        let readiness = evaluate_ship_push_readiness(&snapshot, false);
        assert_eq!(readiness.status, ShipReadinessStatus::NeedsReview);
        assert!(readiness
            .warnings
            .iter()
            .any(|w| w.contains("set upstream")));
    }

    #[test]
    fn ship_push_readiness_blocks_behind_branch_unless_allowed() {
        let mut snapshot = sample_snapshot(Vec::new(), Some(passing_tests()));
        snapshot.branch.ahead = 1;
        snapshot.branch.behind = 1;

        let blocked = evaluate_ship_push_readiness(&snapshot, false);
        assert_eq!(blocked.status, ShipReadinessStatus::Blocked);

        let allowed = evaluate_ship_push_readiness(&snapshot, true);
        assert_eq!(allowed.status, ShipReadinessStatus::Ready);
    }

    #[test]
    fn ship_pr_readiness_requires_upstream_and_pushed_branch() {
        let mut snapshot = sample_snapshot(Vec::new(), Some(passing_tests()));
        snapshot.branch.upstream = None;
        let no_upstream = evaluate_ship_pr_readiness(&snapshot, false);
        assert_eq!(no_upstream.status, ShipReadinessStatus::Blocked);
        assert!(no_upstream
            .blockers
            .iter()
            .any(|b| b.contains("no upstream")));

        snapshot.branch.upstream = Some("origin/feature".into());
        snapshot.branch.ahead = 1;
        let unpushed = evaluate_ship_pr_readiness(&snapshot, false);
        assert_eq!(unpushed.status, ShipReadinessStatus::Blocked);
        assert!(unpushed.blockers.iter().any(|b| b.contains("unpushed")));
    }

    #[test]
    fn ship_pr_readiness_warns_without_tests() {
        let snapshot = sample_snapshot(Vec::new(), None);
        let readiness = evaluate_ship_pr_readiness(&snapshot, false);
        assert_eq!(readiness.status, ShipReadinessStatus::NeedsReview);
        assert!(readiness
            .warnings
            .iter()
            .any(|w| w.contains("tests were not run")));
    }

    #[test]
    fn commit_specific_blockers_match_staging_mode() {
        let clean = sample_snapshot(Vec::new(), Some(passing_tests()));
        assert!(commit_specific_blockers(&clean, ShipStaging::All)[0]
            .contains("no working-tree changes"));

        let unstaged = sample_snapshot(vec![tracked_change("src/main.rs")], Some(passing_tests()));
        assert!(
            commit_specific_blockers(&unstaged, ShipStaging::StagedOnly)[0]
                .contains("no staged changes")
        );

        let staged = sample_snapshot(
            vec![GitFileState {
                path: "src/main.rs".into(),
                original_path: None,
                staged: Some('M'),
                unstaged: Some('.'),
                kind: GitFileKind::Tracked,
            }],
            Some(passing_tests()),
        );
        assert!(commit_specific_blockers(&staged, ShipStaging::StagedOnly).is_empty());
    }

    #[test]
    fn staged_ship_context_uses_only_cached_diff() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("README.md"), "staged\n").unwrap();
        git(dir.path(), &["add", "README.md"]);
        fs::write(dir.path().join("later.txt"), "unstaged\n").unwrap();

        let snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();
        let context = build_staged_ship_context(&snapshot).unwrap().unwrap();

        assert!(context.content.contains("Staged Diff"));
        assert!(context.content.contains("staged"));
        assert!(!context.content.contains("unstaged"));
    }

    #[test]
    fn write_ship_commit_record_creates_markdown() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("README.md"), "changed\n").unwrap();
        git(dir.path(), &["add", "README.md"]);
        let snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();

        let path = write_ship_commit_record(
            dir.path().join(".sessions").to_str().unwrap(),
            &snapshot,
            Some(&passing_tests()),
            ShipStaging::StagedOnly,
            "feat: test ship record",
            "abc1234",
        )
        .unwrap();
        let body = fs::read_to_string(path).unwrap();

        assert!(body.contains("# Small Harness Ship Commit"));
        assert!(body.contains("feat: test ship record"));
        assert!(body.contains("abc1234"));
        assert!(body.contains("Final Staged Diff Stat"));
    }

    #[test]
    fn create_git_commit_commits_staged_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("README.md"), "changed\n").unwrap();
        git(dir.path(), &["add", "README.md"]);

        let hash =
            create_git_commit(dir.path().to_str().unwrap(), "feat: commit from ship").unwrap();
        let subject =
            run_git_capture(dir.path().to_str().unwrap(), &["log", "-1", "--pretty=%s"]).unwrap();

        assert!(!hash.is_empty());
        assert_eq!(subject.trim(), "feat: commit from ship");
    }

    #[test]
    fn resolves_push_target_from_existing_upstream() {
        let mut snapshot = sample_snapshot(Vec::new(), Some(passing_tests()));
        snapshot.branch.ahead = 1;
        snapshot.branch.upstream = Some("origin/release/ship".into());
        let target = resolve_ship_push_target("/tmp", &snapshot).unwrap();

        assert_eq!(target.remote, "origin");
        assert_eq!(target.local_branch, "feature");
        assert_eq!(target.remote_branch, "release/ship");
        assert!(!target.set_upstream);
    }

    #[test]
    fn resolves_push_target_without_upstream_to_origin() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let bare = tempfile::tempdir().unwrap();
        Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(bare.path())
            .output()
            .unwrap();
        git(
            dir.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        let mut snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();
        snapshot.branch.upstream = None;

        let target = resolve_ship_push_target(dir.path().to_str().unwrap(), &snapshot).unwrap();

        assert_eq!(target.remote, "origin");
        assert_eq!(target.local_branch, snapshot.branch.head.unwrap());
        assert!(target.set_upstream);
    }

    #[test]
    fn execute_ship_push_sets_upstream_on_bare_remote() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let bare = tempfile::tempdir().unwrap();
        Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(bare.path())
            .output()
            .unwrap();
        git(
            dir.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        );
        let snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();
        let target = resolve_ship_push_target(dir.path().to_str().unwrap(), &snapshot).unwrap();

        let output = execute_ship_push(dir.path().to_str().unwrap(), &target).unwrap();
        let upstream = run_git_capture(
            dir.path().to_str().unwrap(),
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )
        .unwrap();

        assert!(output.contains("branch"));
        assert!(upstream.trim().starts_with("origin/"));
    }

    #[test]
    fn write_ship_push_record_creates_markdown() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();
        let target = ShipPushTarget {
            remote: "origin".into(),
            local_branch: "feature".into(),
            remote_branch: "feature".into(),
            set_upstream: true,
        };

        let path = write_ship_push_record(
            dir.path().join(".sessions").to_str().unwrap(),
            &snapshot,
            &target,
            "abc1234",
            "pushed",
        )
        .unwrap();
        let body = fs::read_to_string(path).unwrap();

        assert!(body.contains("# Small Harness Ship Push"));
        assert!(body.contains("abc1234"));
        assert!(body.contains("pushed"));
        assert!(body.contains("Set upstream"));
    }

    #[test]
    fn parses_github_remote_urls() {
        assert_eq!(
            parse_github_repo("https://github.com/GetSmallAI/SmallHarness.git").as_deref(),
            Some("GetSmallAI/SmallHarness")
        );
        assert_eq!(
            parse_github_repo("git@github.com:GetSmallAI/SmallHarness.git").as_deref(),
            Some("GetSmallAI/SmallHarness")
        );
        assert_eq!(
            parse_github_repo("ssh://git@github.com/GetSmallAI/SmallHarness.git").as_deref(),
            Some("GetSmallAI/SmallHarness")
        );
        assert!(parse_github_repo("https://example.com/x/y.git").is_none());
    }

    #[test]
    fn resolves_pr_target_from_github_upstream() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        git(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:GetSmallAI/SmallHarness.git",
            ],
        );
        let mut snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();
        snapshot.branch.head = Some("feature".into());
        snapshot.branch.upstream = Some("origin/feature".into());

        let target =
            resolve_ship_pr_target(dir.path().to_str().unwrap(), &snapshot, Some("main")).unwrap();

        assert_eq!(target.repo, "GetSmallAI/SmallHarness");
        assert_eq!(target.head_branch, "feature");
        assert_eq!(target.base_branch, "main");
    }

    #[test]
    fn build_gh_pr_command_adds_draft_by_default() {
        let target = ShipPrTarget {
            remote: "origin".into(),
            repo: "GetSmallAI/SmallHarness".into(),
            head_branch: "feature".into(),
            base_branch: "main".into(),
        };
        let command = build_gh_pr_command(&target, "Title", "Body text", true);

        assert_eq!(command[0], "gh");
        assert!(command.contains(&"--draft".to_string()));
        assert!(shell_command_display(&command).contains("'Body text'"));

        let ready = build_gh_pr_command(&target, "Title", "Body text", false);
        assert!(!ready.contains(&"--draft".to_string()));
    }

    #[test]
    fn render_ship_pr_body_includes_branch_commit_and_tests() {
        let snapshot = sample_snapshot(Vec::new(), Some(passing_tests()));
        let target = ShipPrTarget {
            remote: "origin".into(),
            repo: "GetSmallAI/SmallHarness".into(),
            head_branch: "feature".into(),
            base_branch: "main".into(),
        };
        let body = render_ship_pr_body(&snapshot, &target, "abc123", "abc123 subject");

        assert!(body.contains("abc123"));
        assert!(body.contains("feature"));
        assert!(body.contains("main"));
        assert!(body.contains("Passed"));
    }

    #[test]
    fn write_ship_pr_record_creates_markdown() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();
        let target = ShipPrTarget {
            remote: "origin".into(),
            repo: "GetSmallAI/SmallHarness".into(),
            head_branch: "feature".into(),
            base_branch: "main".into(),
        };
        let command = build_gh_pr_command(&target, "Title", "Body", true);

        let path = write_ship_pr_record(
            dir.path().join(".sessions").to_str().unwrap(),
            &ShipPrRecord {
                snapshot: &snapshot,
                target: &target,
                title: "Title",
                body: "Body",
                command: &command,
                url: Some("https://github.com/GetSmallAI/SmallHarness/pull/1"),
                status: "created",
            },
        )
        .unwrap();
        let body = fs::read_to_string(path).unwrap();

        assert!(body.contains("# Small Harness Ship PR"));
        assert!(body.contains("GetSmallAI/SmallHarness"));
        assert!(body.contains("https://github.com/GetSmallAI/SmallHarness/pull/1"));
        assert!(body.contains("gh pr create"));
    }

    #[test]
    fn extract_url_finds_pr_url() {
        assert_eq!(
            extract_url("Created pull request https://github.com/a/b/pull/1").as_deref(),
            Some("https://github.com/a/b/pull/1")
        );
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
    fn parses_plan_args_variants() {
        assert!(matches!(
            parse_plan_args("show"),
            Some(PlanInvocation::Show)
        ));
        assert!(matches!(
            parse_plan_args("validate"),
            Some(PlanInvocation::Validate)
        ));
        assert!(matches!(
            parse_plan_args("validate the csv export"),
            Some(PlanInvocation::Draft { .. })
        ));

        let Some(PlanInvocation::Draft {
            intent,
            export_path,
        }) = parse_plan_args("add a CSV export command")
        else {
            panic!("expected draft");
        };
        assert_eq!(intent, "add a CSV export command");
        assert!(export_path.is_none());

        let Some(PlanInvocation::Draft {
            intent,
            export_path,
        }) = parse_plan_args("build a dashboard --export /tmp/out.md")
        else {
            panic!("expected draft");
        };
        assert_eq!(intent, "build a dashboard");
        assert_eq!(export_path.as_deref(), Some(Path::new("/tmp/out.md")));

        let Some(PlanInvocation::Draft { export_path, .. }) =
            parse_plan_args("thing --export=/tmp/x.md")
        else {
            panic!("expected draft");
        };
        assert_eq!(export_path.as_deref(), Some(Path::new("/tmp/x.md")));

        assert!(parse_plan_args("").is_none());
        assert!(parse_plan_args("   ").is_none());
        assert!(parse_plan_args("intent --export").is_none());
        assert!(parse_plan_args("--export=/tmp/x.md").is_none());
    }

    #[test]
    fn render_validate_report_marks_met_and_unmet() {
        let criteria = vec![
            "retries on 5xx".to_string(),
            "retries are logged".to_string(),
        ];
        let out = render_validate_report(&criteria, &[true, false]);
        assert!(out.contains("✓"));
        assert!(out.contains("✗"));
        assert!(out.contains("retries on 5xx"));
        assert!(out.contains("retries are logged"));
        assert!(out.contains("1/2 criteria met"));
    }

    #[tokio::test]
    async fn plan_writes_fallback_spec_when_model_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path());
        state.backend = BackendDescriptor {
            name: BackendName::Ollama,
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "test".into(),
            is_local: true,
        };

        cmd_plan("add a CSV export command", &state).await.unwrap();

        let body = fs::read_to_string(dir.path().join(".small-harness/spec.md")).unwrap();
        for section in [
            "## Goal",
            "## User Outcomes",
            "## Scope",
            "## Out of Scope",
            "## Done Criteria",
            "## Open Questions",
        ] {
            assert!(body.contains(section), "missing {section}");
        }
        assert!(body.contains("add a CSV export command"));
        assert!(body.contains("model draft failed"));
    }

    #[tokio::test]
    async fn plan_export_overrides_default_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path());
        state.backend = BackendDescriptor {
            name: BackendName::Ollama,
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "test".into(),
            is_local: true,
        };
        let out_path = dir.path().join("nested/custom-spec.md");

        cmd_plan(
            &format!("build a dashboard --export {}", out_path.display()),
            &state,
        )
        .await
        .unwrap();

        assert!(out_path.exists());
        assert!(!dir.path().join(".small-harness/spec.md").exists());
        assert!(fs::read_to_string(out_path)
            .unwrap()
            .contains("## Done Criteria"));
    }

    #[tokio::test]
    async fn plan_show_without_spec_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        cmd_plan("show", &state).await.unwrap();
        assert!(!dir.path().join(".small-harness/spec.md").exists());
    }
}

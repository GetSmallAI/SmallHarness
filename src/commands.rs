use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::agent::to_openai_tools;
use crate::backends::{
    backend, default_model, validate, BackendDescriptor, BackendName, ProfileName,
};
use crate::budget::{format_bytes, measure_prompt_budget};
use crate::capabilities::{
    self, best_record, recommended_tool_selection, record_score, sorted_records,
    warmup_recommended, BenchmarkStats, CapabilityRecord, CapabilityStatus,
};
use crate::config::{is_tool_name, AgentConfig, ToolSelection, ALL_TOOL_NAMES};
use crate::input::plain_read_line;
use crate::openai::{
    list_models, stream_chat, ChatMessage, ChatRequest, StreamOptions, ToolDef, ToolDefFunction,
};
use crate::session::{
    list_sessions, load_messages, load_session, render_markdown, resolve_session_path,
    save_message, SessionEntry,
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
    ("/context", "Show or update context limits"),
    ("/compact", "Summarize older conversation turns"),
    ("/doctor", "Check backend, tools, env, and session storage"),
    ("/bench", "Measure warmup, first-token, and total latency"),
    ("/capabilities", "Show or refresh model capability cache"),
    (
        "/autotune",
        "Recommend and optionally apply the best cached model",
    ),
    ("/eval", "Run prompt/model comparison suite"),
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
        "/session" => session_info(state),
        "/sessions" => cmd_sessions(state)?,
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
        "/eval" => cmd_eval(&args, state).await?,
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

fn session_info(state: &AppState) {
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
}

fn cmd_sessions(state: &AppState) -> Result<()> {
    let sessions = list_sessions(&state.session_dir)?;
    if sessions.is_empty() {
        println!("  {DIM}No sessions saved yet.{RESET}");
        return Ok(());
    }
    for session in sessions.into_iter().take(20) {
        println!(
            "  {CYAN}{}{RESET} {DIM}{} messages · {} bytes · {}{RESET}",
            session.id,
            session.messages,
            session.bytes,
            format_system_time(session.modified)
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
        println!("  {GREEN}✓{RESET} {DIM}tool selection →{RESET} {CYAN}auto{RESET}");
        return;
    } else if args == "fixed" {
        state.config.tool_selection = ToolSelection::Fixed;
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
            }
        }
    }
    let last_prompt = last_user_prompt(state).unwrap_or_default();
    let active_tool_names = select_tool_names(&state.config, &last_prompt);
    let system_prompt = state
        .config
        .render_system_prompt_for_tools(&active_tool_names);
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
        format_bytes(budget.total_bytes),
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
        "  {DIM}limits{RESET}    maxMessages={:?} maxBytes={:?}",
        state.config.context.max_messages, state.config.context.max_bytes
    );
}

async fn summarize_messages(state: &AppState, older: &[ChatMessage]) -> Result<String> {
    let transcript = serde_json::to_string(older)?;
    let messages = vec![
        ChatMessage::System {
            content: "Summarize this Small Harness conversation for continuing context. Preserve goals, decisions, files touched, errors, and pending work. Be concise.".into(),
        },
        ChatMessage::User {
            content: transcript,
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
        max_tokens: None,
    };
    let mut out = String::new();
    stream_chat(&state.http, &state.backend, &req, None, |chunk| {
        if let Some(choice) = chunk.choices.first() {
            if let Some(content) = &choice.delta.content {
                out.push_str(content);
            }
        }
    })
    .await?;
    Ok(out)
}

async fn cmd_compact(args: &str, state: &mut AppState) -> Result<()> {
    let keep = if args.is_empty() {
        state.config.context.max_messages.unwrap_or(12).clamp(4, 80)
    } else {
        args.parse::<usize>().unwrap_or(12).clamp(4, 80)
    };
    if state.messages.len() <= keep + 1 {
        println!("  {DIM}Nothing to compact yet.{RESET}");
        return Ok(());
    }
    let split_at = state.messages.len().saturating_sub(keep);
    let older = state.messages[..split_at].to_vec();
    let recent: Vec<ChatMessage> = state.messages[split_at..]
        .iter()
        .filter(|m| !matches!(m, ChatMessage::System { .. }))
        .cloned()
        .collect();
    println!("  {DIM}Compacting {} older messages…{RESET}", older.len());
    let summary = summarize_messages(state, &older).await?;
    let mut messages = vec![ChatMessage::System {
        content: format!(
            "{}\n\nConversation summary:\n{}",
            state.config.render_system_prompt(),
            summary.trim()
        ),
    }];
    messages.extend(recent);
    state.messages = messages;
    state.reset_session();
    for message in &state.messages {
        let _ = save_message(&state.session_path, message);
    }
    println!(
        "  {GREEN}✓{RESET} {DIM}compacted to {} messages → {}{RESET}",
        state.messages.len(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

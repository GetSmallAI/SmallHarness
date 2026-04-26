use anyhow::{anyhow, Result};
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use crate::agent::to_openai_tools;
use crate::backends::{
    backend, default_model, validate, BackendDescriptor, BackendName, ProfileName,
};
use crate::config::{is_tool_name, AgentConfig, ALL_TOOL_NAMES};
use crate::input::plain_read_line;
use crate::openai::{list_models, stream_chat, ChatMessage, ChatRequest, StreamOptions};
use crate::session::{
    list_sessions, load_messages, load_session, render_markdown, resolve_session_path,
    save_message, SessionEntry,
};
use crate::tools::build_tools;
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
        "/doctor" => cmd_doctor(state).await?,
        "/bench" => cmd_bench(&args, state).await?,
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
        println!("  {DIM}usage{RESET}      /tools file_read,grep,list_dir");
        return;
    }
    let requested: Vec<String> = args
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
    println!(
        "  {GREEN}✓{RESET} {DIM}tools →{RESET} {CYAN}{}{RESET}",
        state.config.tools.join(", ")
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
    println!("  {DIM}messages{RESET}  {}", state.messages.len());
    println!(
        "  {DIM}bytes{RESET}     {}",
        transcript_bytes(&state.messages)
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

async fn cmd_doctor(state: &AppState) -> Result<()> {
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
    let cps = if total.as_secs_f64() > 0.0 {
        chars as f64 / total.as_secs_f64()
    } else {
        0.0
    };
    println!(
        "  {GREEN}✓{RESET} {DIM}warmup={:?} firstToken={:.2}s total={:.2}s charsPerSec={:.1}{RESET}",
        warm_ms.map(|ms| format!("{:.2}s", ms as f64 / 1000.0)),
        first.map(|d| d.as_secs_f64()).unwrap_or(0.0),
        total.as_secs_f64(),
        cps
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

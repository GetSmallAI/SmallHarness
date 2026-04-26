use anyhow::Result;
use std::path::PathBuf;

use crate::backends::{backend, default_model, validate, BackendDescriptor, BackendName, ProfileName};
use crate::config::{is_tool_name, AgentConfig, ALL_TOOL_NAMES};
use crate::input::plain_read_line;
use crate::openai::{list_models, stream_chat, ChatMessage, ChatRequest, StreamOptions};

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
            self.config.profile,
            self.config.model_override.as_deref(),
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
    ("/session", "Show session info and token usage"),
    ("/backend", "Switch backend (ollama, lm-studio, mlx, openrouter)"),
    ("/profile", "Switch hardware profile (mac-mini-16gb, mac-studio-32gb)"),
    ("/model", "List models from the current backend and pick one"),
    ("/tools", "Show or set enabled tools (comma-separated names)"),
    (
        "/compare",
        "Run the last user prompt against the OpenRouter cloud (requires OPENROUTER_API_KEY)",
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
        "/new" => cmd_new(state),
        "/clear" => clear_screen(),
        "/session" => session_info(state),
        "/backend" => cmd_backend(&args, state).await?,
        "/profile" => cmd_profile(&args, state).await?,
        "/model" => cmd_model(&args, state).await?,
        "/tools" => cmd_tools(&args, state),
        "/compare" => cmd_compare(&args, state).await?,
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
    println!(
        "  {DIM}session{RESET}   {}",
        state.session_path.display()
    );
    println!("  {DIM}messages{RESET}  {}", state.messages.len());
    println!(
        "  {DIM}tokens{RESET}    {} in · {} out",
        fmt_tokens(state.total_in),
        fmt_tokens(state.total_out)
    );
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
        let prompt = format!(
            "  {DIM}Select (1-{}):{RESET} ",
            BackendName::all().len()
        );
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

async fn cmd_profile(args: &str, state: &mut AppState) -> Result<()> {
    let chosen: Option<ProfileName> = if !args.is_empty() {
        ProfileName::parse(args)
    } else {
        println!(
            "  {DIM}Current:{RESET} {CYAN}{}{RESET}",
            state.config.profile.as_str()
        );
        for (i, p) in ProfileName::all().iter().enumerate() {
            println!("  {DIM}{}){RESET} {}", i + 1, p.as_str());
        }
        let prompt = format!(
            "  {DIM}Select (1-{}):{RESET} ",
            ProfileName::all().len()
        );
        let pick = plain_read_line(prompt).await?.trim().to_string();
        pick.parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| ProfileName::all().get(i).copied())
    };
    let Some(chosen) = chosen else {
        println!("  {DIM}Cancelled.{RESET}");
        return Ok(());
    };
    state.config.profile = chosen;
    state.config.model_override = None;
    state.resolve_model();
    println!(
        "  {GREEN}✓{RESET} {DIM}profile →{RESET} {CYAN}{}{RESET} {DIM}· model →{RESET} {CYAN}{}{RESET}",
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
    for (i, m) in shown.iter().enumerate() {
        println!("  {DIM}{:>2}){RESET} {}", i + 1, m);
    }
    if total > shown.len() {
        println!(
            "  {DIM}…and {} more{RESET}",
            total - shown.len()
        );
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
        println!(
            "  {DIM}available{RESET}  {}",
            ALL_TOOL_NAMES.join(", ")
        );
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
        default_model(&cloud_backend, state.config.profile, None)
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
        stream_options: Some(StreamOptions { include_usage: false }),
        max_tokens: None,
    };
    let mut out = std::io::stdout();
    let result = stream_chat(&state.http, &cloud_backend, &req, |chunk| {
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

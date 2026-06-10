//! Config command group: /config, /backend, /model, /tools, /mode, /verbose,
//! /reasoning, /auth, /login, /logout, /compare, /image.
//! Split out of mod.rs; dispatch lives in mod.rs.

use super::*;

pub(super) fn cmd_config(state: &AppState) {
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
pub(super) fn cmd_mode(args: &str, state: &mut AppState) {
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
pub(super) async fn cmd_auth(args: &str) -> Result<()> {
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
        "login" => {
            let mut login_state = AppStateLoginOnly;
            cmd_login(rest, &mut login_state).await
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
                "  {RED}✗{RESET} {DIM}unknown subcommand: {other} (try: list, set, login, clear){RESET}"
            );
            Ok(())
        }
    }
}

struct AppStateLoginOnly;

pub(super) async fn cmd_login(args: &str, state: &mut impl LoginState) -> Result<()> {
    let provider = if args.trim().is_empty() {
        "openai-codex"
    } else {
        args.trim()
    };
    if !matches!(provider, "openai-codex" | "codex" | "chatgpt") {
        println!(
            "  {RED}✗{RESET} {DIM}unknown login provider: {provider} (try: openai-codex){RESET}"
        );
        return Ok(());
    }

    println!("  {BOLD}ChatGPT / Codex login{RESET}");
    println!(
        "  {DIM}This uses your ChatGPT/Codex subscription OAuth token, not OPENAI_API_KEY.{RESET}"
    );
    println!("  {DIM}1) Browser login (default){RESET}");
    println!("  {DIM}2) Device-code login (headless/SSH){RESET}");
    let pick = plain_read_line(format!("  {DIM}Select [1]: {RESET}")).await?;
    let result = if pick.trim() == "2" || pick.trim().eq_ignore_ascii_case("device") {
        crate::codex_oauth::login_and_save_device_code(state.http()).await
    } else {
        crate::codex_oauth::login_and_save_browser(state.http()).await
    };
    match result {
        Ok(path) => {
            println!(
                "  {GREEN}✓{RESET} {DIM}logged in to openai-codex; saved to {}{RESET}",
                path.display()
            );
            state.after_login()?;
        }
        Err(e) => println!("  {RED}✗{RESET} {DIM}login failed: {e}{RESET}"),
    }
    Ok(())
}

pub(super) trait LoginState {
    fn http(&self) -> &reqwest::Client;
    fn after_login(&mut self) -> Result<()>;
}

impl LoginState for AppState {
    fn http(&self) -> &reqwest::Client {
        &self.http
    }
    fn after_login(&mut self) -> Result<()> {
        if matches!(self.config.backend, BackendName::OpenAiCodex) {
            self.rebuild_client()?;
            self.resolve_model();
        }
        Ok(())
    }
}

impl LoginState for AppStateLoginOnly {
    fn http(&self) -> &reqwest::Client {
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        CLIENT.get_or_init(crate::openai::build_http_client)
    }
    fn after_login(&mut self) -> Result<()> {
        Ok(())
    }
}

pub(super) fn cmd_logout(args: &str) -> Result<()> {
    let provider = if args.trim().is_empty() {
        "openai-codex"
    } else {
        args.trim()
    };
    if !matches!(provider, "openai-codex" | "codex" | "chatgpt") {
        println!(
            "  {RED}✗{RESET} {DIM}unknown logout provider: {provider} (try: openai-codex){RESET}"
        );
        return Ok(());
    }
    let mut store = crate::auth::AuthStore::load();
    if store.clear("openai-codex") {
        store.save()?;
        println!("  {GREEN}✓{RESET} {DIM}cleared openai-codex login{RESET}");
    } else {
        println!("  {DIM}no stored openai-codex login{RESET}");
    }
    Ok(())
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
    if let Some(oauth) = store.get_oauth("openai-codex") {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let status = if oauth.expires > now + 60 {
            "oauth logged in"
        } else {
            "oauth refresh needed"
        };
        let source = oauth
            .account_id
            .as_ref()
            .map(|id| format!("auth file · account {id}"))
            .unwrap_or_else(|| "auth file".into());
        println!(
            "  {:<12} {:<22}  {DIM}{}{RESET}",
            "openai-codex", status, source
        );
    } else {
        println!(
            "  {:<12} {:<22}  {DIM}/login openai-codex{RESET}",
            "openai-codex", "(not logged in)"
        );
    }
    if let Some(path) = auth_file_path() {
        println!("  {DIM}file{RESET}     {}", path.display());
    }
}

pub(super) fn cmd_image(args: &str, state: &mut AppState) {
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

pub(super) fn cmd_reasoning(args: &str, state: &mut AppState) {
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

/// `/verbose [on|off|status]` — switch the tool view between the normal grouped
/// summary and a detailed debug view that prints every tool call with its full
/// arguments and a large result preview.
pub(super) fn cmd_verbose(args: &str, state: &mut AppState) {
    let arg = args.trim().to_lowercase();
    let new_state = match arg.as_str() {
        "" | "status" => {
            let cur = state.renderer.verbose_enabled();
            println!(
                "  {DIM}verbose tool view:{RESET} {} {DIM}(usage: /verbose on|off){RESET}",
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
    state.renderer.set_verbose(new_state);
    println!(
        "  {GREEN}✓{RESET} {DIM}verbose tool view →{RESET} {CYAN}{}{RESET}{DIM} — every tool call with full args + result{RESET}",
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

pub(super) async fn cmd_backend(args: &str, state: &mut AppState) -> Result<()> {
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
    if matches!(chosen, BackendName::OpenAiCodex)
        && crate::auth::AuthStore::load()
            .get_oauth("openai-codex")
            .is_none()
    {
        println!(
            "  {RED}✗{RESET} {DIM}not logged in for openai-codex. Run /login openai-codex to sign in with ChatGPT.{RESET}"
        );
        return Ok(());
    }
    if !chosen.is_local()
        && !matches!(chosen, BackendName::OpenAiCodex)
        && backend(chosen).api_key.is_empty()
    {
        let env_name = match chosen {
            BackendName::Openrouter => "OPENROUTER_API_KEY",
            BackendName::OpenAi => "OPENAI_API_KEY",
            BackendName::OpenAiCodex => "ChatGPT login",
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

pub(super) async fn cmd_model(args: &str, state: &mut AppState) -> Result<()> {
    use std::io::Write;
    if !args.is_empty() {
        let model_override = if matches!(state.config.backend, BackendName::OpenAiCodex) {
            let Some(canonical) = crate::codex_responses::canonical_codex_model(args) else {
                println!(
                    "  {RED}✗{RESET} {DIM}{args} is not supported with ChatGPT/Codex login. Try one of: {}{RESET}",
                    crate::codex_responses::codex_model_list().join(", ")
                );
                return Ok(());
            };
            canonical.to_string()
        } else {
            args.to_string()
        };
        state.config.model_override = Some(model_override);
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

pub(super) fn cmd_tools(args: &str, state: &mut AppState) {
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

pub(super) async fn cmd_compare(args: &str, state: &AppState) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OperatorMode;
    use std::path::Path;

    fn test_state(root: &Path) -> AppState {
        use crate::backends::backend;
        use crate::config::AgentConfig;
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
            config,
        }
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
}

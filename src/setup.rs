use anyhow::Result;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::Duration;

use crate::backends::{backend, default_model, validate, BackendName};
use crate::config::{dotenv_values, layered_env, AgentConfig, ApprovalPolicy, ToolSelection};
use crate::hardware::{detect_hardware_spec, save_hardware_summary};
use crate::input::plain_read_line;
use crate::openai::{build_http_client, chat_oneshot, list_models, ChatMessage, ChatRequest};

// Routed through the shared theme so the wizard matches the rest of the TUI
// and secondary text is readable bright-black instead of ANSI faint.
const RESET: &str = crate::theme::RESET;
const DIM: &str = crate::theme::MUTED;
const BOLD: &str = crate::theme::BOLD;
const CYAN: &str = crate::theme::ACCENT;
const GREEN: &str = crate::theme::SUCCESS;
const YELLOW: &str = crate::theme::WARN;
const RED: &str = crate::theme::ERROR;

const CONFIG_PATH: &str = "agent.config.json";
const NO_WIZARD_ENV: &str = "SMALL_HARNESS_NO_WIZARD";

pub fn setup_disabled() -> bool {
    let dotenv = dotenv_values();
    layered_env(&dotenv, NO_WIZARD_ENV)
        .map(|v| is_truthy(&v))
        .unwrap_or(false)
}

pub fn should_run_first_run_setup(config_path: &Path) -> bool {
    !config_path.exists() && !setup_disabled()
}

pub async fn maybe_run_first_run_setup(base: &AgentConfig) -> Result<Option<AgentConfig>> {
    if should_run_first_run_setup(Path::new(CONFIG_PATH)) {
        let mut first_run_defaults = base.clone();
        let spec = detect_hardware_spec();
        let _ = save_hardware_summary(&first_run_defaults.session_dir, &spec);
        first_run_defaults.approval_policy = ApprovalPolicy::DangerousOnly;
        run_setup_wizard(&first_run_defaults).await
    } else {
        Ok(None)
    }
}

pub async fn run_setup_wizard(base: &AgentConfig) -> Result<Option<AgentConfig>> {
    let pad = crate::theme::PAD;
    println!();
    println!("{pad}{CYAN}{BOLD}Small Harness setup{RESET}");
    println!("{}", crate::theme::rule());
    println!(
        "{pad}{DIM}A few quick questions — I'll write {CONFIG_PATH}. Press Enter to keep the\n{pad}shown default ({CYAN}*{RESET}{DIM}); type q to cancel.{RESET}"
    );
    println!();

    let Some(chosen_backend) = prompt_backend(base.backend).await? else {
        println!("  {DIM}Setup cancelled.{RESET}");
        return Ok(None);
    };

    // Cloud backends need an API key. Collect it now so the end-of-wizard
    // probe succeeds instead of failing on a missing key.
    prompt_api_key(chosen_backend).await?;

    let model_default = default_model(&backend(chosen_backend), None);
    let Some(model_override) = prompt_model(&model_default, base.model_override.as_deref()).await?
    else {
        println!("  {DIM}Setup cancelled.{RESET}");
        return Ok(None);
    };
    let Some(approval_policy) = prompt_approval(base.approval_policy).await? else {
        println!("  {DIM}Setup cancelled.{RESET}");
        return Ok(None);
    };
    let Some(tool_selection) = prompt_tool_selection(base.tool_selection).await? else {
        println!("  {DIM}Setup cancelled.{RESET}");
        return Ok(None);
    };

    let mut config = base.clone();
    config.backend = chosen_backend;
    config.model_override = model_override;
    config.approval_policy = approval_policy;
    config.tool_selection = tool_selection;

    write_agent_config(Path::new(CONFIG_PATH), &config)?;
    println!("  {GREEN}✓{RESET} {DIM}wrote {CONFIG_PATH}{RESET}");
    probe_setup_backend(&config).await;

    Ok(Some(config))
}

pub fn write_agent_config(path: &Path, config: &AgentConfig) -> Result<()> {
    let body = serde_json::to_string_pretty(&setup_config_value(config))?;
    std::fs::write(path, format!("{body}\n"))?;
    Ok(())
}

fn setup_config_value(config: &AgentConfig) -> Value {
    let mut obj = Map::new();
    obj.insert("mode".into(), json!(config.mode.as_str()));
    obj.insert("backend".into(), json!(config.backend.as_str()));
    if let Some(model) = &config.model_override {
        obj.insert("modelOverride".into(), json!(model));
    }
    if config.system_prompt != AgentConfig::default().system_prompt {
        obj.insert("systemPrompt".into(), json!(config.system_prompt));
    }
    obj.insert("maxSteps".into(), json!(config.max_steps));
    obj.insert("sessionDir".into(), json!(config.session_dir));
    obj.insert("workspaceRoot".into(), json!(config.workspace_root));
    obj.insert(
        "outsideWorkspace".into(),
        json!(config.outside_workspace.as_str()),
    );
    obj.insert(
        "approvalPolicy".into(),
        json!(config.approval_policy.as_str()),
    );
    obj.insert("tools".into(), json!(config.tools));
    obj.insert(
        "toolSelection".into(),
        json!(config.tool_selection.as_str()),
    );
    obj.insert("display".into(), json!(&config.display));
    obj.insert("slashCommands".into(), json!(config.slash_commands));
    obj.insert("context".into(), json!(&config.context));
    obj.insert("history".into(), json!(&config.history));
    obj.insert("projectMemory".into(), json!(&config.project_memory));
    Value::Object(obj)
}

async fn prompt_backend(default: BackendName) -> Result<Option<BackendName>> {
    loop {
        println!("  {BOLD}Backend{RESET}");
        for (idx, backend) in BackendName::all().iter().enumerate() {
            let marker = if *backend == default { " *" } else { "" };
            println!(
                "    {DIM}{}){RESET} {}{}",
                idx + 1,
                backend.as_str(),
                marker
            );
        }
        let default_idx = BackendName::all()
            .iter()
            .position(|b| *b == default)
            .map(|i| i + 1)
            .unwrap_or(1);
        let input = plain_read_line(format!(
            "  {CYAN}❯{RESET} {DIM}Select backend [{default_idx}]:{RESET} "
        ))
        .await?;
        let trimmed = input.trim().to_lowercase();
        if is_cancel(&trimmed) {
            return Ok(None);
        }
        if trimmed.is_empty() {
            return Ok(Some(default));
        }
        if let Some(parsed) = BackendName::parse(&trimmed) {
            return Ok(Some(parsed));
        }
        if let Some(parsed) = trimmed
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|idx| BackendName::all().get(idx).copied())
        {
            return Ok(Some(parsed));
        }
        println!("  {YELLOW}!{RESET} {DIM}Unknown backend: {trimmed}{RESET}");
    }
}

/// For a cloud backend, make sure an API key is available. If one is already
/// set (env var or stored auth file, which is hydrated into the env at
/// startup), say so and move on. Otherwise prompt for it and persist it to the
/// `0600` auth file, also setting it in the current process so the end-of-wizard
/// probe works this session. Local backends need no key, so this is a no-op.
async fn prompt_api_key(chosen: BackendName) -> Result<()> {
    use crate::auth::{auth_file_path, env_var_for, mask_key, AuthStore};

    if chosen.is_local() {
        return Ok(());
    }
    let provider = chosen.as_str();
    let Some(env_name) = env_var_for(provider) else {
        return Ok(());
    };

    let existing = std::env::var(env_name).unwrap_or_default();
    if !existing.trim().is_empty() {
        println!(
            "  {GREEN}✓{RESET} {DIM}{provider} key already set ({}){RESET}",
            mask_key(existing.trim())
        );
        return Ok(());
    }

    println!("  {BOLD}API key{RESET}  {DIM}{provider} needs one.{RESET}");
    let key = plain_read_line(format!(
        "  {CYAN}❯{RESET} {DIM}Paste {provider} API key (visible while typing, blank to skip): {RESET}"
    ))
    .await?
    .trim()
    .to_string();

    if key.is_empty() {
        println!(
            "  {YELLOW}!{RESET} {DIM}Skipped — set {env_name} or run /auth set {provider} before your first prompt.{RESET}"
        );
        return Ok(());
    }

    let mut store = AuthStore::load();
    store.set(provider, &key);
    match store.save() {
        Ok(()) => {
            std::env::set_var(env_name, &key);
            let path = auth_file_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(no path)".into());
            println!(
                "  {GREEN}✓{RESET} {DIM}{provider} →{RESET} {CYAN}{}{RESET} {DIM}(saved to {}){RESET}",
                mask_key(&key),
                path
            );
        }
        Err(e) => {
            // Still set it for this session so the probe can succeed.
            std::env::set_var(env_name, &key);
            println!("  {YELLOW}!{RESET} {DIM}saved for this session, but writing the auth file failed: {e}{RESET}");
        }
    }
    Ok(())
}

async fn prompt_model(
    default_model: &str,
    current_override: Option<&str>,
) -> Result<Option<Option<String>>> {
    println!("  {BOLD}Model{RESET}");
    let prompt = if let Some(current) = current_override {
        format!("  {CYAN}❯{RESET} {DIM}Model override [current: {current}; blank: {default_model}]:{RESET} ")
    } else {
        format!("  {CYAN}❯{RESET} {DIM}Model override [blank: {default_model}]:{RESET} ")
    };
    let input = plain_read_line(prompt).await?;
    let trimmed = input.trim();
    if is_cancel(trimmed) {
        return Ok(None);
    }
    if trimmed.is_empty() {
        Ok(Some(None))
    } else {
        Ok(Some(Some(trimmed.to_string())))
    }
}

async fn prompt_approval(default: ApprovalPolicy) -> Result<Option<ApprovalPolicy>> {
    let options = [
        ApprovalPolicy::Always,
        ApprovalPolicy::DangerousOnly,
        ApprovalPolicy::Never,
    ];
    loop {
        println!("  {BOLD}Approval policy{RESET}");
        for (idx, policy) in options.iter().enumerate() {
            let marker = if *policy == default { " *" } else { "" };
            println!("    {DIM}{}){RESET} {}{}", idx + 1, policy.as_str(), marker);
        }
        let default_idx = options
            .iter()
            .position(|p| *p == default)
            .map(|i| i + 1)
            .unwrap_or(1);
        let input = plain_read_line(format!(
            "  {CYAN}❯{RESET} {DIM}Select approval [{default_idx}]:{RESET} "
        ))
        .await?;
        let trimmed = input.trim().to_lowercase();
        if is_cancel(&trimmed) {
            return Ok(None);
        }
        if trimmed.is_empty() {
            return Ok(Some(default));
        }
        if let Some(parsed) = ApprovalPolicy::parse(&trimmed) {
            return Ok(Some(parsed));
        }
        if let Some(parsed) = trimmed
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|idx| options.get(idx).copied())
        {
            return Ok(Some(parsed));
        }
        println!("  {YELLOW}!{RESET} {DIM}Unknown approval policy: {trimmed}{RESET}");
    }
}

async fn prompt_tool_selection(default: ToolSelection) -> Result<Option<ToolSelection>> {
    let options = [ToolSelection::Auto, ToolSelection::Fixed];
    loop {
        println!("  {BOLD}Tool mode{RESET}");
        for (idx, selection) in options.iter().enumerate() {
            let marker = if *selection == default { " *" } else { "" };
            println!(
                "    {DIM}{}){RESET} {}{}",
                idx + 1,
                selection.as_str(),
                marker
            );
        }
        let default_idx = options
            .iter()
            .position(|s| *s == default)
            .map(|i| i + 1)
            .unwrap_or(1);
        let input = plain_read_line(format!(
            "  {CYAN}❯{RESET} {DIM}Select tool mode [{default_idx}]:{RESET} "
        ))
        .await?;
        let trimmed = input.trim().to_lowercase();
        if is_cancel(&trimmed) {
            return Ok(None);
        }
        if trimmed.is_empty() {
            return Ok(Some(default));
        }
        if let Some(parsed) = ToolSelection::parse(&trimmed) {
            return Ok(Some(parsed));
        }
        if let Some(parsed) = trimmed
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|idx| options.get(idx).copied())
        {
            return Ok(Some(parsed));
        }
        println!("  {YELLOW}!{RESET} {DIM}Unknown tool mode: {trimmed}{RESET}");
    }
}

async fn probe_setup_backend(config: &AgentConfig) {
    let http = build_http_client();
    let backend_desc = config.backend_descriptor();
    let model = default_model(&backend_desc, config.model_override.as_deref());
    println!(
        "  {DIM}Probing {} at {} with {CYAN}{}{RESET}{DIM}…{RESET}",
        config.backend.as_str(),
        backend_desc.base_url,
        model
    );

    if let Err(e) = validate(&backend_desc) {
        println!("  {RED}✗{RESET} {DIM}{e}{RESET}");
        println!("  {DIM}Hint: {}{RESET}", backend_hint(config.backend));
        return;
    }

    let models = match with_probe_timeout(list_models(&http, &backend_desc)).await {
        Ok(models) => models,
        Err(e) => {
            println!("  {YELLOW}!{RESET} {DIM}Backend not reachable: {e}{RESET}");
            println!("  {DIM}Hint: {}{RESET}", backend_hint(config.backend));
            return;
        }
    };
    println!(
        "  {GREEN}✓{RESET} {DIM}model list reachable ({} models){RESET}",
        models.len()
    );

    let messages = [ChatMessage::User {
        content: "Reply with ok.".into(),
    }];
    let req = ChatRequest {
        model: &model,
        messages: &messages,
        tools: None,
        stream: false,
        stream_options: None,
        max_tokens: Some(4),
        effort: None,
    };
    match with_probe_timeout(chat_oneshot(&http, &backend_desc, &req)).await {
        Ok(()) => println!("  {GREEN}✓{RESET} {DIM}chat completion reachable{RESET}"),
        Err(e) => {
            println!("  {YELLOW}!{RESET} {DIM}chat probe failed: {e}{RESET}");
            println!(
                "  {DIM}The config is saved. Check the model id or start the backend, then run /doctor.{RESET}"
            );
        }
    }
}

async fn with_probe_timeout<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    match tokio::time::timeout(Duration::from_secs(8), future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!("timed out after 8s"),
    }
}

fn is_cancel(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "q" | "quit" | "cancel")
}

fn is_truthy(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn backend_hint(backend: BackendName) -> &'static str {
    match backend {
        BackendName::Ollama => {
            "run `ollama serve` and pull a model such as `ollama pull qwen2.5-coder:7b`."
        }
        BackendName::LmStudio => {
            "open LM Studio, go to Local Server, load a model, and start the server on port 1234."
        }
        BackendName::Mlx => {
            "start an OpenAI-compatible MLX server, for example `mlx_lm.server --port 8080`."
        }
        BackendName::LlamaCpp => {
            "run `llama-server -m /path/to/model.gguf --host 127.0.0.1 --port 8080 --jinja`."
        }
        BackendName::Openrouter => "set `OPENROUTER_API_KEY` before using the OpenRouter backend.",
        BackendName::OpenAi => {
            "set `OPENAI_API_KEY` before using the OpenAI backend (optionally `OPENAI_BASE_URL` for a compatible proxy)."
        }
        BackendName::OpenAiCodex => {
            "run `/login openai-codex` to sign in with ChatGPT/Codex subscription OAuth."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_truthy_skip_values() {
        assert!(is_truthy("true"));
        assert!(is_truthy("1"));
        assert!(is_truthy("YES"));
        assert!(is_truthy("on"));
        assert!(!is_truthy("false"));
        assert!(!is_truthy(""));
    }

    #[test]
    fn setup_config_json_uses_public_contract_names() {
        let config = AgentConfig {
            backend: BackendName::LlamaCpp,
            model_override: Some("local-gguf".into()),
            approval_policy: ApprovalPolicy::DangerousOnly,
            tool_selection: ToolSelection::Fixed,
            ..Default::default()
        };

        let value = setup_config_value(&config);

        assert_eq!(value["backend"], "llamacpp");
        assert_eq!(value["modelOverride"], "local-gguf");
        assert_eq!(value["approvalPolicy"], "dangerous-only");
        assert_eq!(value["toolSelection"], "fixed");
        assert_eq!(value["outsideWorkspace"], "prompt");
        assert_eq!(value["slashCommands"], true);
        assert_eq!(value["projectMemory"]["enabled"], true);
        assert!(value.get("systemPrompt").is_none());
    }

    #[test]
    fn first_run_setup_requires_missing_config_and_enabled_wizard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.config.json");
        let previous = std::env::var(NO_WIZARD_ENV).ok();

        std::env::set_var(NO_WIZARD_ENV, "false");
        assert!(should_run_first_run_setup(&path));

        std::fs::write(&path, "{}").unwrap();
        assert!(!should_run_first_run_setup(&path));

        if let Some(value) = previous {
            std::env::set_var(NO_WIZARD_ENV, value);
        } else {
            std::env::remove_var(NO_WIZARD_ENV);
        }
    }
}

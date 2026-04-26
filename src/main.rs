mod agent;
mod approval;
mod backends;
mod banner;
mod cancel;
mod commands;
mod config;
mod input;
mod loader;
mod openai;
mod renderer;
mod session;
mod tools;
mod warmup;

use std::io::IsTerminal;

use crate::agent::{run_agent, AgentEvent, ApprovalProvider};
use crate::approval::ApprovalCache;
use crate::backends::{backend, default_model, validate, BackendDescriptor};
use crate::banner::{print_banner, BannerInfo};
use crate::cancel::CancellationToken;
use crate::commands::{dispatch, AppState};
use crate::config::{load_config, InputStyle};
use crate::input::{bordered_read_line, plain_read_line_with_history, InputHistory};
use crate::loader::Loader;
use crate::openai::{build_http_client, list_models, ChatMessage};
use crate::renderer::TuiRenderer;
use crate::session::{init_session_dir, new_session_path, save_message};
use crate::tools::build_tools;
use crate::warmup::warmup;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const GRAY: &str = "\x1b[90m";

fn format_tokens(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        n.to_string()
    }
}

async fn probe_backend(http: &reqwest::Client, b: &BackendDescriptor) -> Result<(), String> {
    match list_models(http, b).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let hint = match b.name {
                crate::backends::BackendName::Ollama => {
                    "Is `ollama serve` running? Default port is 11434."
                }
                crate::backends::BackendName::LmStudio => {
                    "Open LM Studio → \"Local Server\" tab → Start Server. Default port is 1234."
                }
                crate::backends::BackendName::Mlx => {
                    "Start an MLX OpenAI-compatible server (e.g. `mlx_lm.server`). Default port is 8080."
                }
                crate::backends::BackendName::Openrouter => "Check OPENROUTER_API_KEY.",
            };
            Err(format!("{e}. {hint}"))
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "small-harness requires an interactive TTY (run it directly in a terminal, not piped)."
        );
        std::process::exit(1);
    }
    let config = load_config();
    let http = build_http_client();
    let backend_desc = backend(config.backend);
    if let Err(e) = validate(&backend_desc) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    let model = default_model(
        &backend_desc,
        &config.profile,
        config.model_override.as_deref(),
        &config.profiles,
    );

    if config.display.show_banner {
        print_banner(BannerInfo {
            backend: config.backend.as_str(),
            profile: config.profile.as_str(),
            model: &model,
            approval: config.approval_policy.as_str(),
        });
    }

    let probe = probe_backend(&http, &backend_desc).await;
    if let Err(hint) = probe {
        println!("  {YELLOW}!{RESET} {DIM}Backend not reachable: {hint}{RESET}");
        println!("  {DIM}You can still type /backend to switch, or fix and retry.{RESET}");
    } else if std::env::var("WARMUP").as_deref() != Ok("false") {
        let warmup_tools_vec = build_tools(&config);
        let warmup_tool_defs = crate::agent::to_openai_tools(&warmup_tools_vec);
        let warmup_prompt = config.render_system_prompt();
        let loader = Loader::start("Warming up".into(), config.display.loader_style);
        match warmup(
            &http,
            &backend_desc,
            &model,
            &warmup_prompt,
            &warmup_tool_defs,
        )
        .await
        {
            Ok(ms) => {
                loader.stop();
                println!(
                    "  {DIM}warmed up in {:.1}s — first prompt should be fast{RESET}",
                    ms as f64 / 1000.0
                );
            }
            Err(e) => {
                loader.stop();
                println!("  {DIM}warmup skipped: {e}{RESET}");
            }
        }
    }

    init_session_dir(&config.session_dir)?;
    let mut input_history = InputHistory::load(
        config.history_path(),
        config.history.max_entries,
        config.history.enabled,
    );
    let session_path = new_session_path(&config.session_dir);
    let session_dir = config.session_dir.clone();

    let mut state = AppState {
        config,
        http,
        backend: backend_desc,
        model,
        messages: Vec::<ChatMessage>::new(),
        session_dir,
        session_path,
        total_in: 0,
        total_out: 0,
    };

    let mut approval_cache = ApprovalCache::new();
    let mut renderer = TuiRenderer::new(state.config.display.clone());

    loop {
        let input = match state.config.display.input_style {
            InputStyle::Bordered => bordered_read_line(input_history.entries().to_vec()).await?,
            _ => {
                plain_read_line_with_history(
                    format!("{GREEN}>{RESET} "),
                    input_history.entries().to_vec(),
                )
                .await?
            }
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = input_history.push(&input);

        if matches!(state.config.display.input_style, InputStyle::Bordered) {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let home = std::env::var("HOME").unwrap_or_default();
            let display_cwd = if !home.is_empty() && cwd.starts_with(&home) {
                cwd.replacen(&home, "~", 1)
            } else {
                cwd
            };
            println!("  {DIM}{display_cwd}{RESET}");
        }

        if trimmed == "exit" || trimmed == "quit" || trimmed == ".exit" {
            println!("  {DIM}bye.{RESET}");
            std::process::exit(0);
        }

        if state.config.slash_commands && trimmed.starts_with('/') {
            if let Err(e) = dispatch(trimmed, &mut state).await {
                println!("  {RED}✗{RESET} {DIM}{e}{RESET}");
            }
            continue;
        }

        if state.messages.is_empty() {
            let sys = ChatMessage::System {
                content: state.config.render_system_prompt(),
            };
            state.messages.push(sys.clone());
            let _ = save_message(&state.session_path, &sys);
        }
        let user_msg = ChatMessage::User {
            content: trimmed.to_string(),
        };
        state.messages.push(user_msg.clone());
        let _ = save_message(&state.session_path, &user_msg);

        let tools = build_tools(&state.config);
        let initial = state.messages.clone();
        let max_steps = state.config.max_steps;
        let model = state.model.clone();
        let backend_desc_clone = state.backend.clone();
        let http_clone = state.http.clone();

        let loader = Loader::start(
            state.config.display.loader_text.clone(),
            state.config.display.loader_style,
        );
        let mut loader_opt = Some(loader);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let cancel = CancellationToken::new();
        let cancel_for_agent = cancel.clone();
        let cancel_for_signal = cancel.clone();
        let ctrl_task = tokio::spawn(async move {
            let mut hits = 0usize;
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                hits += 1;
                if hits == 1 {
                    cancel_for_signal.cancel();
                    eprintln!("\n  cancelling current turn… press Ctrl-C again to exit");
                } else {
                    std::process::exit(130);
                }
            }
        });

        let agent_fut = async {
            let on_event = move |e: AgentEvent| {
                let _ = tx.send(e);
            };
            run_agent(
                &http_clone,
                &backend_desc_clone,
                &model,
                initial,
                tools,
                max_steps,
                on_event,
                Some(&mut approval_cache as &mut dyn ApprovalProvider),
                Some(cancel_for_agent),
            )
            .await
        };

        let drain_fut = async {
            while let Some(e) = rx.recv().await {
                if let Some(l) = loader_opt.take() {
                    l.stop();
                }
                renderer.handle(e);
            }
        };

        let before = state.messages.len();
        let (result, _) = tokio::join!(agent_fut, drain_fut);
        ctrl_task.abort();

        if let Some(l) = loader_opt.take() {
            l.stop();
        }
        renderer.end_turn();

        match result {
            Ok(res) => {
                state.messages = res.messages;
                for i in before..state.messages.len() {
                    let _ = save_message(&state.session_path, &state.messages[i]);
                }
                state.total_in += res.input_tokens;
                state.total_out += res.output_tokens;
                println!(
                    "{GRAY}  {} in · {} out{RESET}",
                    format_tokens(res.input_tokens),
                    format_tokens(res.output_tokens)
                );
            }
            Err(e) => {
                println!("  {RED}✗{RESET} {DIM}{e}{RESET}");
            }
        }
    }
}

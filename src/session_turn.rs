use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::hash::{Hash, Hasher};

use crate::agent::{run_agent, AgentEvent, ApprovalProvider, RunResult};
use crate::app_state::AppState;
use crate::backends::BackendDescriptor;
use crate::cancel::CancellationToken;
use crate::catalog::{format_usd, turn_cost_usd};
use crate::config::OperatorMode;
use crate::context_guard::{
    maybe_auto_compact, merge_system_prompt, rewrite_session_transcript, CompactSessionContext,
};
use crate::loader::Loader;
use crate::openai::{ChatMessage, ImageUrl, UserContent, UserContentPart};
use crate::project_memory::{refresh_project_memory_after_write, render_system_prompt_with_memory};
use crate::session::save_message;
use crate::shipcheck::{append_ship_context, collect_shipcheck};
use crate::test_integration::{
    format_test_failure_feedback, run_selected_tests, smart_test_selection, TestResult,
};
use crate::tools::{
    build_tools_for_names, select_tool_names, tool_output_mutated_workspace, ToolPreview,
};
use crate::turn_checkpoint::{active_tools_need_checkpoints, should_push_checkpoint, TurnCapturer};
use crate::warmup::warmup;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const GRAY: &str = "\x1b[90m";

pub struct TurnOptions {
    pub user_prompt: String,
    pub auto_verify_tests: bool,
    pub yolo_approve: bool,
}

#[allow(dead_code)]
pub struct TurnOutcome {
    pub run_result: RunResult,
    pub memory_changed: bool,
    pub last_test_result: Option<TestResult>,
    pub checkpoint_pushed: bool,
    pub tool_calls: Vec<String>,
}

struct YoloApproval;

#[async_trait]
impl ApprovalProvider for YoloApproval {
    async fn approve(
        &mut self,
        _name: &str,
        _args: &Value,
        _preview: Option<&ToolPreview>,
    ) -> bool {
        true
    }
}

fn format_tokens(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Build the cost suffix for the end-of-turn status line.
///
/// Renders nothing for purely local sessions so users on Ollama/LM Studio
/// don't see a meaningless "$0.00." For cloud sessions: shows the turn cost
/// when the model is in the catalog, "$?" when it isn't (e.g. OpenRouter
/// or a not-yet-cataloged OpenAI model), and prefixes the session total
/// with `≥` whenever any turn fell into the unknown bucket.
fn format_path_suffix(state: &AppState) -> String {
    if !state.paths_enabled() || state.path_store.path_count() <= 1 {
        return String::new();
    }
    format!(
        " · path: {} · {} paths",
        state.path_store.active_id(),
        state.path_store.path_count()
    )
}

fn format_cost_suffix(
    turn_cost: Option<f64>,
    backend_is_local: bool,
    session_usd: f64,
    has_unknown: bool,
) -> String {
    if backend_is_local && session_usd == 0.0 && !has_unknown {
        return String::new();
    }
    let turn_part = match turn_cost {
        Some(c) => format_usd(c),
        None if backend_is_local => format_usd(0.0),
        None => "$?".into(),
    };
    let session_prefix = if has_unknown { "≥" } else { "" };
    format!(
        " · {turn_part} this turn · {session_prefix}{} session",
        format_usd(session_usd)
    )
}

fn prompt_fingerprint(
    backend: &BackendDescriptor,
    model: &str,
    system_prompt: &str,
    tool_names: &[String],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    backend.name.hash(&mut hasher);
    backend.base_url.hash(&mut hasher);
    model.hash(&mut hasher);
    system_prompt.hash(&mut hasher);
    tool_names.hash(&mut hasher);
    hasher.finish()
}

fn set_system_message(messages: &mut Vec<ChatMessage>, system_prompt: String) -> bool {
    if let Some(ChatMessage::System { content }) = messages.first_mut() {
        *content = system_prompt;
        false
    } else {
        messages.insert(
            0,
            ChatMessage::System {
                content: system_prompt,
            },
        );
        true
    }
}

pub async fn run_user_turn(state: &mut AppState, opts: TurnOptions) -> Result<TurnOutcome> {
    let trimmed = opts.user_prompt.trim();
    if trimmed.is_empty() {
        anyhow::bail!("turn prompt is empty");
    }

    let active_tool_names = select_tool_names(&state.config, trimmed);
    let base_system_prompt = append_ship_context(
        &render_system_prompt_with_memory(
            &state.config,
            &state.backend,
            &active_tool_names,
            trimmed,
        ),
        &state.config,
        state.tests_ran_this_session,
    );
    let mut system_prompt =
        merge_system_prompt(&base_system_prompt, state.conversation_summary.as_deref());
    if set_system_message(&mut state.messages, system_prompt.clone()) {
        if let Some(sys) = state.messages.first() {
            let _ = save_message(&state.session_path, sys);
        }
    }
    let content = if state.pending_image_attachments.is_empty() {
        UserContent::Text(trimmed.to_string())
    } else {
        let mut parts: Vec<UserContentPart> = Vec::new();
        parts.push(UserContentPart::Text {
            text: trimmed.to_string(),
        });
        for url in state.pending_image_attachments.drain(..) {
            parts.push(UserContentPart::ImageUrl {
                image_url: ImageUrl { url },
            });
        }
        UserContent::Parts(parts)
    };
    let user_msg = ChatMessage::User { content };
    state.messages.push(user_msg.clone());
    let _ = save_message(&state.session_path, &user_msg);

    let mut tools = build_tools_for_names(&state.config, &active_tool_names);
    tools.extend(state.mcp_tools.iter().cloned());
    let tool_defs = crate::agent::to_openai_tools(&tools);
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
    if let Some(notice) = maybe_auto_compact(
        &mut compact_ctx,
        &state.session_dir,
        &mut state.session_path,
    )
    .await?
    {
        println!("{}", notice.line);
        if let Some(summary) = notice.conversation_summary {
            state.conversation_summary = Some(summary);
        }
        state.context_guard_notice = Some(
            notice
                .line
                .trim()
                .trim_start_matches("\x1b[32m✓\x1b[0m \x1b[2m")
                .trim_end_matches("\x1b[0m")
                .to_string(),
        );
    }
    system_prompt = merge_system_prompt(&base_system_prompt, state.conversation_summary.as_deref());
    if let Some(ChatMessage::System { content }) = state.messages.first_mut() {
        *content = system_prompt.clone();
    }
    let guard_params = crate::context_guard::guard_params_from(
        &state.config,
        &state.model,
        state.backend.is_local,
        state.conversation_summary.clone(),
    );
    let fingerprint = prompt_fingerprint(
        &state.backend,
        &state.model,
        &system_prompt,
        &active_tool_names,
    );
    if std::env::var("WARMUP").as_deref() != Ok("false")
        && state.warmed_fingerprint != Some(fingerprint)
    {
        let loader = Loader::start(
            "Warming prompt cache".into(),
            state.config.display.loader_style,
        );
        let warm_result = warmup(
            &state.http,
            &state.backend,
            &state.model,
            &system_prompt,
            &tool_defs,
        )
        .await;
        loader.stop();
        if warm_result.is_ok() {
            state.warmed_fingerprint = Some(fingerprint);
        }
    }

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

    let mut turn_capturer =
        if state.checkpoints_enabled && active_tools_need_checkpoints(&active_tool_names) {
            Some(TurnCapturer::new(
                state.config.workspace_root.clone(),
                state.config.checkpoints.limits(),
            ))
        } else {
            None
        };

    let mut yolo = YoloApproval;
    let approval: &mut dyn ApprovalProvider = if opts.yolo_approve {
        &mut yolo
    } else {
        &mut state.approval_cache
    };

    let mut tool_calls = Vec::new();
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
            Some(approval),
            Some(cancel_for_agent),
            Some((guard_params, base_system_prompt.clone())),
            turn_capturer.as_mut(),
        )
        .await
    };

    let mut memory_changed = false;
    let drain_fut = async {
        while let Some(e) = rx.recv().await {
            if let Some(l) = loader_opt.take() {
                l.stop();
            }
            if let AgentEvent::ToolCall { name, .. } = &e {
                tool_calls.push(name.clone());
            }
            if let AgentEvent::ToolResult { name, output, .. } = &e {
                if tool_output_mutated_workspace(name, output) {
                    memory_changed = true;
                }
            }
            if let AgentEvent::ContextCompacted {
                notice,
                conversation_summary,
            } = &e
            {
                state.context_guard_notice = Some(notice.clone());
                if let Some(summary) = conversation_summary {
                    state.conversation_summary = Some(summary.clone());
                }
            }
            state.renderer.handle(e);
        }
    };

    let before = state.messages.len();
    let (result, _) = tokio::join!(agent_fut, drain_fut);
    ctrl_task.abort();

    if let Some(l) = loader_opt.take() {
        l.stop();
    }
    state.renderer.end_turn();

    let res = result?;
    state.messages = res.messages.clone();
    if let Some(summary) = res.conversation_summary.clone() {
        state.conversation_summary = Some(summary);
    }
    if res.transcript_rewritten || state.messages.len() < before {
        if let Err(e) =
            rewrite_session_transcript(&state.session_dir, &mut state.session_path, &state.messages)
        {
            println!("  {RED}✗{RESET} {DIM}session rewrite failed: {e}{RESET}");
        }
    } else {
        for message in &state.messages[before..] {
            let _ = save_message(&state.session_path, message);
        }
    }
    state.total_in += res.input_tokens;
    state.total_out += res.output_tokens;
    let turn_cost = turn_cost_usd(
        state.config.backend,
        &state.model,
        res.input_tokens,
        res.output_tokens,
    );
    match turn_cost {
        Some(c) => state.session_usd += c,
        None => {
            // Local backends have no $ cost — silently treat as zero, don't
            // mark the session total as a lower bound. Cloud backends with
            // an uncataloged model are the real "unknown" case.
            if !state.config.backend.is_local() && (res.input_tokens > 0 || res.output_tokens > 0) {
                state.session_cost_has_unknown = true;
            }
        }
    }

    let mut checkpoint_pushed = false;
    let mut last_test_result = None;

    if memory_changed {
        state.path_store.mark_dirty();
        if let Some(capturer) = turn_capturer {
            let checkpoint = capturer.into_checkpoint();
            if should_push_checkpoint(true, &checkpoint) {
                let files = checkpoint.file_count();
                state.checkpoint_stack.push(checkpoint);
                checkpoint_pushed = true;
                println!("  {DIM}checkpoint saved ({files} file(s)) — /undo to revert{RESET}");
            }
        }
        match refresh_project_memory_after_write(&state.config) {
            Ok(Some(index)) => println!(
                "  {DIM}project memory refreshed ({} files){RESET}",
                index.files.len()
            ),
            Ok(None) => {}
            Err(e) => {
                println!("  {YELLOW}!{RESET} {DIM}project memory refresh skipped: {e}{RESET}")
            }
        }
        if opts.auto_verify_tests && state.config.mode == OperatorMode::Ship {
            if let Ok(selected) = smart_test_selection(&state.config.workspace_root) {
                if !selected.is_empty() {
                    match run_selected_tests(&state.config.workspace_root, &selected) {
                        Ok(result) => {
                            state.tests_ran_this_session = true;
                            last_test_result = Some(result.clone());
                            if result.failed > 0 || result.exit_code != 0 {
                                let feedback = format_test_failure_feedback(&result);
                                let verify_msg = ChatMessage::User {
                                    content: feedback.into(),
                                };
                                state.messages.push(verify_msg.clone());
                                let _ = save_message(&state.session_path, &verify_msg);
                                println!(
                                    "  {YELLOW}tests:{RESET} {} failed (see context)",
                                    result.failed
                                );
                            } else if let Ok(snapshot) =
                                collect_shipcheck(&state.config.workspace_root)
                            {
                                if snapshot.ready_to_ship() {
                                    println!("  {GREEN}✓{RESET} {DIM}ready for /handoff{RESET}");
                                }
                            }
                        }
                        Err(e) => {
                            println!("  {YELLOW}!{RESET} {DIM}auto-verify skipped: {e}{RESET}")
                        }
                    }
                }
            }
        }
    }

    println!(
        "{GRAY}  {} in · {} out{}{}{RESET}",
        format_tokens(res.input_tokens),
        format_tokens(res.output_tokens),
        format_cost_suffix(
            turn_cost,
            state.config.backend.is_local(),
            state.session_usd,
            state.session_cost_has_unknown
        ),
        format_path_suffix(state),
    );

    Ok(TurnOutcome {
        run_result: res,
        memory_changed,
        last_test_result,
        checkpoint_pushed,
        tool_calls,
    })
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    #[test]
    fn no_suffix_for_pure_local_session() {
        assert_eq!(format_cost_suffix(None, true, 0.0, false), "");
    }

    #[test]
    fn known_turn_renders_both_parts() {
        let s = format_cost_suffix(Some(0.0003), false, 0.0003, false);
        assert!(s.contains("$0.0003 this turn"));
        assert!(s.contains("$0.0003 session"));
        assert!(!s.contains("≥"));
    }

    #[test]
    fn unknown_cloud_turn_marks_session_as_lower_bound() {
        let s = format_cost_suffix(None, false, 0.5, true);
        assert!(s.contains("$? this turn"));
        assert!(s.contains("≥$0.50 session"));
    }

    #[test]
    fn local_turn_after_cloud_history_shows_zero_not_unknown() {
        let s = format_cost_suffix(None, true, 0.42, false);
        assert!(s.contains("$0.00 this turn"));
        assert!(s.contains("$0.42 session"));
        assert!(!s.contains("≥"));
    }
}

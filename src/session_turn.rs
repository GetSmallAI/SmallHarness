use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::agent::{run_agent, AgentEvent, ApprovalProvider, RunResult};
use crate::app_state::AppState;
use crate::backends::BackendDescriptor;
use crate::budget::{format_bytes, headroom_bytes, measure_prompt_budget, usage_ratio};
use crate::cancel::CancellationToken;
use crate::catalog::{format_usd, turn_cost_usd};
use crate::config::OperatorMode;
use crate::context_guard::{
    guard_config_from, maybe_auto_compact, merge_system_prompt, rewrite_session_transcript,
    CompactSessionContext,
};
use crate::loader::Loader;
use crate::model_system::EffortLevel;
use crate::openai::{ChatMessage, ImageUrl, UserContent, UserContentPart};
use crate::project_memory::{refresh_project_memory_after_write, render_system_prompt_with_memory};
use crate::session::save_message;
use crate::shipcheck::{append_ship_context, collect_shipcheck};
use crate::test_integration::{
    format_test_failure_feedback, run_selected_tests, smart_test_selection, TestResult,
};
use crate::tools::{
    build_tools_for_names, select_tool_names, tool_output_mutated_workspace, ToolPreview,
    ToolRuntimeContext,
};
use crate::turn_checkpoint::{active_tools_need_checkpoints, should_push_checkpoint, TurnCapturer};
use crate::turn_trace::{ApprovalDecision, TracePayload, TurnMetrics};
use crate::warmup::warmup;

const RESET: &str = crate::theme::RESET;
const DIM: &str = crate::theme::MUTED;
const GREEN: &str = crate::theme::SUCCESS;
const YELLOW: &str = crate::theme::WARN;
const RED: &str = crate::theme::ERROR;
const GRAY: &str = crate::theme::MUTED;

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

struct TracingApproval<'a> {
    inner: &'a mut crate::approval::ApprovalCache,
    trace: crate::turn_trace::SharedTurnTrace,
    approval_ms: std::cell::Cell<u128>,
}

#[async_trait]
impl ApprovalProvider for TracingApproval<'_> {
    async fn approve(&mut self, name: &str, args: &Value, preview: Option<&ToolPreview>) -> bool {
        let start = Instant::now();
        let cache_key = format!(
            "{name}:{}",
            args.get("command")
                .and_then(Value::as_str)
                .or_else(|| args.get("path").and_then(Value::as_str))
                .unwrap_or("")
        );
        let allowed = self.inner.approve(name, args, preview).await;
        let elapsed = start.elapsed().as_millis();
        self.approval_ms.set(self.approval_ms.get() + elapsed);
        let decision = if allowed {
            ApprovalDecision::Allowed
        } else {
            ApprovalDecision::Denied
        };
        if let Ok(guard) = self.trace.lock() {
            let _ = guard.append(TracePayload::Approval {
                tool: name.to_string(),
                decision,
                cache_key,
                duration_ms: elapsed,
            });
        }
        allowed
    }
}

fn format_tokens(n: u32) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        n.to_string()
    }
}

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

fn format_timing_suffix(metrics: &TurnMetrics) -> String {
    metrics.format_footer_suffix()
}

fn format_effort_suffix(effort: Option<EffortLevel>) -> String {
    effort
        .map(|effort| format!(" · effort {}", effort.as_str()))
        .unwrap_or_default()
}

fn prompt_fingerprint(
    backend: &BackendDescriptor,
    model: &str,
    effort: Option<EffortLevel>,
    system_prompt: &str,
    tool_names: &[String],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    backend.name.hash(&mut hasher);
    backend.base_url.hash(&mut hasher);
    model.hash(&mut hasher);
    effort.hash(&mut hasher);
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

fn maybe_print_context_pressure(
    state: &AppState,
    system_prompt: &str,
    tool_defs: &[crate::openai::ToolDef],
) {
    let guard = guard_config_from(&state.config, &state.model, state.backend.is_local);
    let budget = measure_prompt_budget(system_prompt, &state.messages, tool_defs);
    let ratio = usage_ratio(&budget, guard.effective_limit_bytes);
    let threshold = guard.compact_threshold * 0.7;
    if ratio < threshold {
        return;
    }
    println!(
        "  {DIM}context {:.0}% · schema {} · headroom {}{RESET}",
        ratio * 100.0,
        format_bytes(budget.tool_schema_bytes),
        format_bytes(headroom_bytes(&budget, guard.effective_limit_bytes)),
    );
}

pub async fn run_user_turn(state: &mut AppState, opts: TurnOptions) -> Result<TurnOutcome> {
    let trimmed = opts.user_prompt.trim();
    if trimmed.is_empty() {
        anyhow::bail!("turn prompt is empty");
    }

    if let Ok(mut trace) = state.trace.lock() {
        trace.begin_turn();
    }
    state.renderer.set_trace(state.trace_enabled);

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

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let tool_runtime = ToolRuntimeContext {
        trace: state.trace.clone(),
        trace_enabled: state.trace_enabled,
        agent_events: Some(tx.clone()),
    };
    let mut tools = build_tools_for_names(&state.config, &active_tool_names, Some(&tool_runtime));
    tools.extend(state.mcp_tools.iter().cloned());
    let tool_defs = crate::agent::to_openai_tools(&tools);
    maybe_print_context_pressure(state, &system_prompt, &tool_defs);

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
        state.active_effort,
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
            state.active_effort,
            &system_prompt,
            &tool_defs,
        )
        .await;
        loader.stop();
        if let Ok(ms) = warm_result {
            state.warmed_fingerprint = Some(fingerprint);
            println!(
                "  {DIM}re-warming prompt cache (backend/model/tools changed) · {:.0}ms{RESET}",
                ms
            );
            if let Ok(trace) = state.trace.lock() {
                let _ = trace.append(TracePayload::Warmup {
                    duration_ms: ms,
                    reason: "fingerprint_changed".into(),
                });
            }
        }
    }

    let initial = state.messages.clone();
    let max_steps = state.config.max_steps;
    let model = state.model.clone();
    let active_effort = state.active_effort;
    let backend_desc_clone = state.backend.clone();
    let http_clone = state.http.clone();
    let trace = state.trace.clone();

    let loader = Loader::start(
        state.config.display.loader_text.clone(),
        state.config.display.loader_style,
    );
    let mut loader_opt = Some(loader);

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
    let mut tracing_approval = TracingApproval {
        inner: &mut state.approval_cache,
        trace: state.trace.clone(),
        approval_ms: std::cell::Cell::new(0),
    };
    let approval: &mut dyn ApprovalProvider = if opts.yolo_approve {
        &mut yolo
    } else {
        &mut tracing_approval
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
            active_effort,
            initial,
            tools,
            max_steps,
            on_event,
            Some(approval),
            Some(cancel_for_agent),
            Some((guard_params, base_system_prompt.clone())),
            turn_capturer.as_mut(),
            Some(trace),
            0,
        )
        .await
    };

    let mut memory_changed = false;
    let loader_style = state.config.display.loader_style;
    let default_loader_text = state.config.display.loader_text.clone();
    let drain_fut = async {
        while let Some(e) = rx.recv().await {
            if let Some(l) = loader_opt.take() {
                l.stop();
            }
            if let AgentEvent::ToolCall { name, .. } = &e {
                tool_calls.push(name.clone());
                if let Some(loader) = loader_opt.as_mut() {
                    loader.set_text(format!("Running {name}…"));
                } else {
                    loader_opt = Some(Loader::start(format!("Running {name}…"), loader_style));
                }
            }
            if let AgentEvent::ToolResult { .. } = &e {
                if let Some(loader) = loader_opt.as_mut() {
                    loader.set_text(default_loader_text.clone());
                }
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
    let mut metrics = res.metrics.clone();
    if !opts.yolo_approve {
        metrics.approval_ms = tracing_approval.approval_ms.get();
    }
    metrics.total_ms = metrics
        .total_ms
        .max(metrics.model_ms + metrics.tool_ms + metrics.approval_ms);

    if let Ok(trace) = state.trace.lock() {
        let _ = trace.log_turn_summary(metrics.clone());
    }

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
        let _ = state.reset_trace_for_session();
    } else {
        for message in &state.messages[before..] {
            let _ = save_message(&state.session_path, message);
        }
    }
    state.total_in += res.input_tokens;
    state.total_out += res.output_tokens;
    if let Err(e) = crate::scorecard::record_turn(crate::scorecard::TurnRecordInput {
        workspace_root: &state.config.workspace_root,
        session_path: &state.session_path,
        backend: state.config.backend.as_str(),
        model: &state.model,
        input_tokens: res.input_tokens,
        output_tokens: res.output_tokens,
        enabled: state.config.scorecard.enabled,
    }) {
        if state.renderer.verbose_enabled() {
            println!("  {YELLOW}!{RESET} {DIM}scorecard turn not recorded: {e}{RESET}");
        }
    }
    let turn_cost = res.reported_cost_usd.or_else(|| {
        turn_cost_usd(
            state.config.backend,
            &state.model,
            res.input_tokens,
            res.output_tokens,
        )
    });
    match turn_cost {
        Some(c) => state.session_usd += c,
        None => {
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

    let scorecard_suffix = crate::scorecard::format_scorecard_suffix(
        &state.config.workspace_root,
        state.config.scorecard.enabled,
        state.config.scorecard.nudge_min_turns,
    )
    .unwrap_or_default();

    println!(
        "{GRAY}  {} in · {} out{}{}{}{}{}{RESET}",
        format_tokens(res.input_tokens),
        format_tokens(res.output_tokens),
        format_cost_suffix(
            turn_cost,
            state.config.backend.is_local(),
            state.session_usd,
            state.session_cost_has_unknown
        ),
        format_effort_suffix(state.active_effort),
        format_timing_suffix(&metrics),
        format_path_suffix(state),
        scorecard_suffix,
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
    use crate::turn_trace::TurnMetrics;

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
    fn provider_reported_cost_can_render_dynamic_router_costs() {
        let s = format_cost_suffix(Some(0.0123), false, 0.0123, false);
        assert!(s.contains("$0.01 this turn"));
        assert!(s.contains("$0.01 session"));
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

    #[test]
    fn effort_suffix_renders_when_set() {
        assert_eq!(format_effort_suffix(None), "");
        assert_eq!(
            format_effort_suffix(Some(EffortLevel::High)),
            " · effort high"
        );
    }

    #[test]
    fn timing_suffix_includes_steps_and_model_time() {
        let metrics = TurnMetrics {
            steps: 3,
            ttft_ms: Some(500),
            model_ms: 1200,
            tool_ms: 800,
            approval_ms: 0,
            total_ms: 2500,
            hit_step_limit: false,
        };
        let s = format_timing_suffix(&metrics);
        assert!(s.contains("3 steps"));
        assert!(s.contains("model"));
    }
}

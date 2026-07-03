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
    should_compact, CompactSessionContext,
};
use crate::hooks::{
    dispatch_hook_payload, hook_context_messages, HookEventName, HookInvocationContext, HookNotice,
    HookOutcome,
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

const RESET: crate::theme::Style = crate::theme::RESET;
const DIM: crate::theme::Style = crate::theme::MUTED;
const GREEN: crate::theme::Style = crate::theme::SUCCESS;
const YELLOW: crate::theme::Style = crate::theme::WARN;
const RED: crate::theme::Style = crate::theme::ERROR;
const GRAY: crate::theme::Style = crate::theme::MUTED;

pub struct TurnOptions {
    pub user_prompt: String,
    pub auto_verify_tests: bool,
    pub yolo_approve: bool,
    pub source: &'static str,
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
    approval_ms: std::cell::Cell<u64>,
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
        let elapsed = start.elapsed().as_millis() as u64;
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

/// Turn cost above which the cost part of the footer is highlighted in
/// WARN instead of the footer's usual muted gray. Deliberately not
/// configurable — it's a visual nudge, not a budget control.
const TURN_COST_WARN_USD: f64 = 0.50;

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
        "path: {} · {} paths",
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
    let text = format!(
        "{turn_part} this turn · {session_prefix}{} session",
        format_usd(session_usd)
    );
    if turn_cost.is_some_and(|c| c > TURN_COST_WARN_USD) {
        format!("{YELLOW}{text}{GRAY}")
    } else {
        text
    }
}

fn format_timing_suffix(metrics: &TurnMetrics) -> String {
    metrics.format_footer_suffix()
}

fn format_effort_suffix(effort: Option<EffortLevel>) -> String {
    effort
        .map(|effort| format!("effort {}", effort.as_str()))
        .unwrap_or_default()
}

/// Join the non-empty footer parts with a single `" · "` separator, so
/// individual suffix builders don't each bake in their own leading
/// separator (which used to make empty-part handling error-prone).
#[allow(clippy::too_many_arguments)]
fn format_footer(
    input_tokens: u32,
    output_tokens: u32,
    turn_cost: Option<f64>,
    backend_is_local: bool,
    session_usd: f64,
    session_cost_has_unknown: bool,
    effort: Option<EffortLevel>,
    metrics: &TurnMetrics,
    path_suffix: &str,
    scorecard_suffix: &str,
) -> String {
    let mut parts = vec![
        format!("{} in", format_tokens(input_tokens)),
        format!("{} out", format_tokens(output_tokens)),
    ];
    let cost = format_cost_suffix(
        turn_cost,
        backend_is_local,
        session_usd,
        session_cost_has_unknown,
    );
    if !cost.is_empty() {
        parts.push(cost);
    }
    let effort_part = format_effort_suffix(effort);
    if !effort_part.is_empty() {
        parts.push(effort_part);
    }
    let timing = format_timing_suffix(metrics);
    if !timing.is_empty() {
        parts.push(timing);
    }
    if !path_suffix.is_empty() {
        parts.push(path_suffix.to_string());
    }
    if !scorecard_suffix.is_empty() {
        parts.push(scorecard_suffix.to_string());
    }
    format!("{GRAY}  {}{RESET}", parts.join(" · "))
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

fn hook_context_from_state(state: &AppState, source: &str) -> HookInvocationContext {
    let turn_id = state
        .trace
        .lock()
        .map(|trace| trace.current_turn())
        .unwrap_or(0);
    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| state.workspace_root())
        .display()
        .to_string();
    HookInvocationContext {
        session_id: state
            .session_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("session")
            .to_string(),
        turn_id,
        cwd,
        workspace_root: state.workspace_root().display().to_string(),
        transcript_path: state.session_path.display().to_string(),
        events_path: crate::turn_trace::events_path_for_session(&state.session_path)
            .display()
            .to_string(),
        backend: state.backend.name.as_str().into(),
        model: state.model.clone(),
        approval_policy: state.config.approval_policy.as_str().into(),
        source: source.into(),
    }
}

fn merge_payload_fields(mut payload: Value, fields: Option<Value>) -> Value {
    let Some(Value::Object(extra)) = fields else {
        return payload;
    };
    let Some(obj) = payload.as_object_mut() else {
        return payload;
    };
    for (key, value) in extra {
        obj.insert(key, value);
    }
    payload
}

fn render_hook_notices(renderer: &mut crate::renderer::TuiRenderer, notices: &[HookNotice]) {
    for notice in notices {
        renderer.handle(AgentEvent::HookNotice(notice.clone()));
    }
}

fn append_hook_contexts(contexts: &mut Vec<String>, event: HookEventName, outcome: &HookOutcome) {
    contexts.extend(hook_context_messages(event, outcome));
}

fn queue_hook_context(state: &mut AppState, event: HookEventName, outcome: &HookOutcome) {
    append_hook_contexts(&mut state.pending_hook_contexts, event, outcome);
}

pub async fn dispatch_app_hook(
    state: &mut AppState,
    event: HookEventName,
    fields: Option<Value>,
    matcher_value: Option<&str>,
) -> HookOutcome {
    dispatch_app_hook_with_source(state, "interactive", event, fields, matcher_value).await
}

pub async fn dispatch_app_hook_with_source(
    state: &mut AppState,
    source: &str,
    event: HookEventName,
    fields: Option<Value>,
    matcher_value: Option<&str>,
) -> HookOutcome {
    let ctx = hook_context_from_state(state, source);
    let payload = merge_payload_fields(ctx.payload(event).into_value(), fields);
    let outcome = dispatch_hook_payload(
        &state.hooks,
        event,
        &payload,
        matcher_value,
        Some(state.trace.clone()),
    )
    .await;
    render_hook_notices(&mut state.renderer, &outcome.notices);
    outcome
}

pub(crate) fn updated_prompt_from_hook_input(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string).or_else(|| {
        value
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

pub(crate) fn system_prompt_with_hook_context(
    mut system_prompt: String,
    contexts: &[String],
) -> String {
    if contexts.is_empty() {
        return system_prompt;
    }
    system_prompt.push_str("\n\nAdditional context from hooks:\n");
    for context in contexts {
        system_prompt.push_str("- ");
        system_prompt.push_str(&crate::hooks::bounded_hook_context_text(context));
        system_prompt.push('\n');
    }
    system_prompt
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
    let hook_source = opts.source;
    let mut user_prompt = opts.user_prompt.trim().to_string();
    if user_prompt.is_empty() {
        anyhow::bail!("turn prompt is empty");
    }

    if let Ok(mut trace) = state.trace.lock() {
        trace.begin_turn();
    }
    state.renderer.set_trace(state.trace_enabled);

    let prompt_hook_outcome = dispatch_app_hook_with_source(
        state,
        hook_source,
        HookEventName::UserPromptSubmit,
        Some(serde_json::json!({ "prompt": user_prompt.clone() })),
        None,
    )
    .await;
    if let Some(reason) = prompt_hook_outcome.blocking_reason {
        anyhow::bail!("UserPromptSubmit hook blocked prompt: {reason}");
    }
    if let Some(reason) = prompt_hook_outcome.stop_reason {
        anyhow::bail!("UserPromptSubmit hook stopped prompt: {reason}");
    }
    if let Some(updated) = prompt_hook_outcome
        .updated_input
        .as_ref()
        .and_then(updated_prompt_from_hook_input)
    {
        user_prompt = updated.trim().to_string();
        if user_prompt.is_empty() {
            anyhow::bail!("UserPromptSubmit hook rewrote prompt to empty");
        }
    }
    let trimmed = user_prompt.as_str();

    let active_tool_names = select_tool_names(&state.config, trimmed);
    let raw_base_system_prompt = append_ship_context(
        &render_system_prompt_with_memory(
            &state.config,
            &state.backend,
            &active_tool_names,
            trimmed,
        ),
        &state.config,
        state.tests_ran_this_session,
    );
    let mut hook_prompt_contexts = Vec::new();
    hook_prompt_contexts.extend(state.session_hook_contexts.clone());
    hook_prompt_contexts.append(&mut state.pending_hook_contexts);
    hook_prompt_contexts.extend(hook_context_messages(
        HookEventName::UserPromptSubmit,
        &prompt_hook_outcome,
    ));
    let mut base_system_prompt =
        system_prompt_with_hook_context(raw_base_system_prompt.clone(), &hook_prompt_contexts);
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
    let drain_hook_registry = state.hooks.clone();
    let drain_hook_trace = state.trace.clone();
    let drain_hook_context = hook_context_from_state(state, hook_source);
    let agent_hooks = crate::agent::AgentHooks {
        registry: drain_hook_registry,
        context: drain_hook_context,
        trace: drain_hook_trace,
    };
    let tool_runtime = ToolRuntimeContext {
        trace: state.trace.clone(),
        trace_enabled: state.trace_enabled,
        agent_events: Some(tx.clone()),
        hooks: Some(agent_hooks.clone()),
    };
    let mut tools = build_tools_for_names(&state.config, &active_tool_names, Some(&tool_runtime));
    tools.extend(state.mcp_tools.iter().cloned());
    // The runtime context owns an event-sender clone for nested tools. Keeping
    // this outer clone alive across the drain join prevents the channel from
    // closing after the agent future returns.
    drop(tool_runtime);
    let tool_defs = crate::agent::to_openai_tools(&tools);
    maybe_print_context_pressure(state, &system_prompt, &tool_defs);

    let compact_guard = guard_config_from(&state.config, &state.model, state.backend.is_local);
    let active_compact_prompt =
        merge_system_prompt(&base_system_prompt, state.conversation_summary.as_deref());
    let compact_budget = measure_prompt_budget(&active_compact_prompt, &state.messages, &tool_defs);
    let auto_compact_due = compact_guard.auto_compact
        && should_compact(
            &compact_budget,
            compact_guard.effective_limit_bytes,
            compact_guard.compact_threshold,
        );
    let compact_allowed = if auto_compact_due {
        let pre_compact = dispatch_app_hook_with_source(
            state,
            hook_source,
            HookEventName::PreCompact,
            Some(serde_json::json!({
                "phase": "turn",
                "message_count": state.messages.len(),
            })),
            None,
        )
        .await;
        if let Some(reason) = pre_compact.stop_reason {
            anyhow::bail!("PreCompact hook stopped turn: {reason}");
        }
        hook_prompt_contexts.extend(hook_context_messages(
            HookEventName::PreCompact,
            &pre_compact,
        ));
        base_system_prompt =
            system_prompt_with_hook_context(raw_base_system_prompt.clone(), &hook_prompt_contexts);
        pre_compact.blocking_reason.is_none()
    } else {
        true
    };
    if compact_allowed {
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
            let summary = notice.conversation_summary.clone();
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
            let post_compact = dispatch_app_hook_with_source(
                state,
                hook_source,
                HookEventName::PostCompact,
                Some(serde_json::json!({
                    "phase": "turn",
                    "notice": notice.line,
                    "conversation_summary": summary,
                    "message_count": state.messages.len(),
                })),
                None,
            )
            .await;
            hook_prompt_contexts.extend(hook_context_messages(
                HookEventName::PostCompact,
                &post_compact,
            ));
            base_system_prompt = system_prompt_with_hook_context(
                raw_base_system_prompt.clone(),
                &hook_prompt_contexts,
            );
        }
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
                    duration_ms: ms as u64,
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
            Some(agent_hooks),
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
    let stop_outcome = dispatch_app_hook_with_source(
        state,
        hook_source,
        HookEventName::Stop,
        Some(serde_json::json!({
            "metrics": metrics.clone(),
            "input_tokens": res.input_tokens,
            "output_tokens": res.output_tokens,
            "hit_step_limit": res.hit_step_limit,
        })),
        None,
    )
    .await;
    queue_hook_context(state, HookEventName::Stop, &stop_outcome);
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

    // One leading blank separates the quiet stats footer from the turn's
    // response and any checkpoint/test notices above it.
    println!();
    println!(
        "{}",
        format_footer(
            res.input_tokens,
            res.output_tokens,
            turn_cost,
            state.config.backend.is_local(),
            state.session_usd,
            state.session_cost_has_unknown,
            state.active_effort,
            &metrics,
            &format_path_suffix(state),
            &scorecard_suffix,
        )
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
    use crate::app_state::AppState;
    use crate::approval::ApprovalCache;
    use crate::backends::{BackendDescriptor, BackendName};
    use crate::config::AgentConfig;
    use crate::hooks::HookRegistry;
    use crate::openai::{build_http_client, ChatMessage};
    use crate::renderer::TuiRenderer;
    use crate::session::load_messages;
    use crate::session_paths::PathStore;
    use crate::turn_checkpoint::CheckpointStack;
    use crate::turn_trace::TurnMetrics;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn mock_backend(listener: &TcpListener) -> BackendDescriptor {
        BackendDescriptor {
            name: BackendName::Ollama,
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            api_key: "test".into(),
            is_local: true,
            openrouter: crate::backends::OpenRouterConfig::default(),
        }
    }

    fn spawn_mock_server(listener: TcpListener, body: &'static str) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        })
    }

    fn test_state(config: AgentConfig, backend: BackendDescriptor) -> AppState {
        let session_dir = config.session_dir.clone();
        let session_path = crate::session::new_session_path(&session_dir);
        let checkpoint_limits = config.checkpoints.limits();
        let display = config.display.clone();
        let paths_config = config.paths.clone();
        let trace = crate::turn_trace::test_trace_for(&session_path);
        AppState {
            config,
            http: build_http_client(),
            backend,
            model: "mock".into(),
            active_effort: None,
            messages: Vec::new(),
            session_dir: session_dir.clone(),
            session_path: session_path.clone(),
            total_in: 0,
            total_out: 0,
            session_usd: 0.0,
            session_cost_has_unknown: false,
            context_guard_notice: None,
            conversation_summary: None,
            checkpoint_stack: CheckpointStack::new(checkpoint_limits),
            checkpoints_enabled: false,
            play_session: None,
            last_play_scorecard: None,
            approval_cache: ApprovalCache::new(),
            renderer: TuiRenderer::new(display),
            hooks: HookRegistry::default(),
            session_hook_contexts: Vec::new(),
            pending_hook_contexts: Vec::new(),
            warmed_fingerprint: None,
            tests_ran_this_session: false,
            pending_image_attachments: Vec::new(),
            mcp_tools: Vec::new(),
            path_store: PathStore::new(&session_dir, &session_path, &paths_config),
            trace,
            trace_enabled: false,
        }
    }

    #[tokio::test]
    async fn user_turn_finishes_after_plain_streaming_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let backend = mock_backend(&listener);
        let server = spawn_mock_server(
            listener,
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello from mock.\"}}]}\n\n",
                "data: [DONE]\n\n"
            ),
        );
        let dir = tempfile::tempdir().unwrap();
        let mut config = AgentConfig {
            backend: BackendName::Ollama,
            model_override: Some("mock".into()),
            workspace_root: dir.path().display().to_string(),
            session_dir: dir.path().join(".sessions").display().to_string(),
            ..Default::default()
        };
        config.tools.clear();
        config.mcp_servers.clear();
        config.display.event_log.enabled = false;
        crate::session::init_session_dir(&config.session_dir).unwrap();
        let mut state = test_state(config, backend);
        let _warmup = EnvVarGuard::set("WARMUP", "false");

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_user_turn(
                &mut state,
                TurnOptions {
                    user_prompt: "hello".into(),
                    auto_verify_tests: false,
                    yolo_approve: false,
                    source: "test",
                },
            ),
        )
        .await
        .expect("turn should finish after the agent stream closes")
        .unwrap();

        server.join().unwrap();
        assert!(result.run_result.messages.iter().any(|message| matches!(
            message,
            ChatMessage::Assistant { content: Some(content), .. } if content == "Hello from mock."
        )));
        let saved = load_messages(&state.session_path).unwrap();
        assert!(saved.iter().any(|message| matches!(
            message,
            ChatMessage::Assistant { content: Some(content), .. } if content == "Hello from mock."
        )));
    }

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
        assert_eq!(format_effort_suffix(Some(EffortLevel::High)), "effort high");
    }

    #[test]
    fn cost_above_warn_threshold_is_highlighted() {
        let s = format_cost_suffix(Some(0.75), false, 0.75, false);
        assert!(s.contains(&YELLOW.to_string()));
    }

    #[test]
    fn cost_below_warn_threshold_is_not_highlighted() {
        let s = format_cost_suffix(Some(0.10), false, 0.10, false);
        assert!(!s.contains(&YELLOW.to_string()));
    }

    #[test]
    fn footer_has_no_doubled_or_leading_separators_when_parts_empty() {
        let metrics = TurnMetrics::default();
        let footer = format_footer(1200, 87, None, true, 0.0, false, None, &metrics, "", "");
        // Only the two always-present parts (tokens in/out) should appear,
        // joined by exactly one " · ", with no trailing/leading separator.
        assert!(footer.contains("1.2k in · 87 out"));
        assert!(!footer.contains("· ·"));
        assert!(!footer
            .trim_start_matches(&GRAY.to_string())
            .starts_with(" · "));
    }

    #[test]
    fn footer_joins_all_present_parts_with_single_separator() {
        let metrics = TurnMetrics {
            steps: 2,
            ttft_ms: None,
            model_ms: 0,
            tool_ms: 0,
            approval_ms: 0,
            total_ms: 1000,
            hit_step_limit: false,
        };
        let footer = format_footer(
            500,
            120,
            Some(0.01),
            false,
            0.01,
            false,
            Some(EffortLevel::High),
            &metrics,
            "path: main · 2 paths",
            "3 turn(s) tracked · /ship pr closes scorecard",
        );
        assert!(footer.contains("500 in · 120 out · $0.01 this turn · $0.01 session · effort high"));
        assert!(footer.contains("path: main · 2 paths"));
        assert!(footer.contains("3 turn(s) tracked"));
    }

    #[test]
    fn local_backend_footer_has_no_cost_part() {
        let metrics = TurnMetrics::default();
        let footer = format_footer(100, 50, None, true, 0.0, false, None, &metrics, "", "");
        assert!(!footer.contains('$'));
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

    #[test]
    fn updated_prompt_accepts_string_or_prompt_field() {
        assert_eq!(
            updated_prompt_from_hook_input(&serde_json::json!("rewritten")),
            Some("rewritten".to_string())
        );
        assert_eq!(
            updated_prompt_from_hook_input(&serde_json::json!({ "prompt": "rewritten again" })),
            Some("rewritten again".to_string())
        );
        assert_eq!(
            updated_prompt_from_hook_input(&serde_json::json!({ "other": "ignored" })),
            None
        );
    }

    #[test]
    fn hook_context_is_appended_to_system_prompt() {
        let merged = system_prompt_with_hook_context(
            "base prompt".to_string(),
            &["first".to_string(), "second".to_string()],
        );
        assert!(merged.starts_with("base prompt"));
        assert!(merged.contains("Additional context from hooks:"));
        assert!(merged.contains("first"));
        assert!(merged.contains("second"));
        assert_eq!(
            system_prompt_with_hook_context("base prompt".to_string(), &[]),
            "base prompt"
        );
    }

    #[test]
    fn hook_context_in_system_prompt_is_redacted_and_bounded() {
        let secret = "sk-secret123456789";
        let merged = system_prompt_with_hook_context(
            "base prompt".to_string(),
            &[format!("{secret} {}", "x".repeat(20_000))],
        );

        assert!(!merged.contains(secret));
        assert!(merged.contains("(redacted)"));
        assert!(merged.contains("[truncated]"));
        assert!(merged.len() < 10_000);
    }

    #[test]
    fn stop_hook_context_can_be_queued_for_next_prompt() {
        let mut outcome = HookOutcome::default();
        outcome.additional_context.push("remember this".into());
        outcome.notices.push(crate::hooks::HookNotice {
            event: HookEventName::Stop,
            hook_key: Some("managed:test:Stop:0:0".into()),
            level: crate::hooks::HookNoticeLevel::Feedback,
            message: "next turn feedback".into(),
        });

        let mut contexts = Vec::new();
        append_hook_contexts(&mut contexts, HookEventName::Stop, &outcome);
        let prompt = system_prompt_with_hook_context("base prompt".to_string(), &contexts);

        assert!(prompt.contains("Stop hook additional context: remember this"));
        assert!(prompt.contains("Stop hook feedback: next turn feedback"));
    }
}

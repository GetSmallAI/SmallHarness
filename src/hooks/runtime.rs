use serde_json::{json, Value};

use super::registry::HookDispatchResult;
use super::{
    HookDecision, HookDispatch, HookEffect, HookEventName, HookInvocationContext, HookRegistry,
};
use crate::turn_trace::{SharedTurnTrace, TracePayload};

const MAX_HOOK_CONTEXT_ITEM_CHARS: usize = 2_000;
const MAX_HOOK_CONTEXT_TOTAL_CHARS: usize = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookNoticeLevel {
    Warning,
    Blocked,
    Denied,
    Stopped,
    Feedback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookNotice {
    pub event: HookEventName,
    pub hook_key: Option<String>,
    pub level: HookNoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct HookOutcome {
    pub blocking_reason: Option<String>,
    pub stop_reason: Option<String>,
    pub allowed: bool,
    pub additional_context: Vec<String>,
    pub updated_input: Option<Value>,
    pub notices: Vec<HookNotice>,
}

pub async fn dispatch_hook_payload(
    registry: &HookRegistry,
    event: HookEventName,
    payload: &Value,
    matcher_value: Option<&str>,
    trace: Option<SharedTurnTrace>,
) -> HookOutcome {
    if let Some(trace) = &trace {
        for hook in registry.matching_hooks(event, matcher_value) {
            append_trace(
                trace,
                TracePayload::HookStart {
                    event: event.as_str().into(),
                    key: hook.key,
                    source: hook.source.label,
                    command: crate::turn_trace::redact_string(&hook.handler.command),
                    matcher: hook.matcher,
                },
            );
        }
    }

    let dispatch = registry.dispatch(event, payload, matcher_value).await;
    if let Some(trace) = &trace {
        for result in &dispatch.results {
            append_trace(
                trace,
                TracePayload::HookEnd {
                    event: event.as_str().into(),
                    key: result.hook.key.clone(),
                    duration_ms: result.run.duration_ms,
                    exit_code: result.run.exit_code,
                    timed_out: result.run.timed_out,
                    stdout: trace_preview(&result.run.stdout),
                    stderr: trace_preview(&result.run.stderr),
                },
            );
            let effect = &result.run.effect;
            let decision = effective_decision(event, result);
            if decision.is_some()
                || effect.feedback.is_some()
                || effect.warning.is_some()
                || effect.reason.is_some()
            {
                append_trace(
                    trace,
                    TracePayload::HookDecision {
                        event: event.as_str().into(),
                        key: result.hook.key.clone(),
                        decision: decision.map(hook_decision_name).map(str::to_string),
                        reason: decision_reason(event, result, decision)
                            .map(|reason| bounded_hook_context_text(&reason)),
                        feedback: effect.feedback.as_deref().map(bounded_hook_context_text),
                        warning: effect.warning.as_deref().map(bounded_hook_context_text),
                    },
                );
            }
        }
    }
    summarize_hook_dispatch(event, &dispatch)
}

pub fn summarize_hook_dispatch(event: HookEventName, dispatch: &HookDispatch) -> HookOutcome {
    let mut outcome = HookOutcome::default();
    for result in &dispatch.results {
        let effect = &result.run.effect;
        if let Some(warning) = &effect.warning {
            outcome.notices.push(HookNotice {
                event,
                hook_key: Some(result.hook.key.clone()),
                level: HookNoticeLevel::Warning,
                message: bounded_hook_context_text(warning),
            });
        }
        let decision = effective_decision(event, result);
        match decision {
            Some(HookDecision::Block) => {
                let raw_reason = decision_reason(event, result, decision)
                    .unwrap_or_else(|| format!("{} hook blocked execution", event.as_str()));
                let reason = bounded_hook_context_text(&raw_reason);
                if outcome.blocking_reason.is_none() {
                    outcome.blocking_reason = Some(reason.clone());
                }
                outcome.notices.push(HookNotice {
                    event,
                    hook_key: Some(result.hook.key.clone()),
                    level: HookNoticeLevel::Blocked,
                    message: reason,
                });
            }
            Some(HookDecision::Deny) => {
                let raw_reason = effect
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("{} hook denied execution", event.as_str()));
                let reason = bounded_hook_context_text(&raw_reason);
                if outcome.blocking_reason.is_none() {
                    outcome.blocking_reason = Some(reason.clone());
                }
                outcome.notices.push(HookNotice {
                    event,
                    hook_key: Some(result.hook.key.clone()),
                    level: HookNoticeLevel::Denied,
                    message: reason,
                });
            }
            Some(HookDecision::Stop) => {
                let raw_reason = effect
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("{} hook stopped the turn", event.as_str()));
                let reason = bounded_hook_context_text(&raw_reason);
                if outcome.stop_reason.is_none() {
                    outcome.stop_reason = Some(reason.clone());
                }
                outcome.notices.push(HookNotice {
                    event,
                    hook_key: Some(result.hook.key.clone()),
                    level: HookNoticeLevel::Stopped,
                    message: reason,
                });
            }
            Some(HookDecision::Allow) => {
                outcome.allowed = true;
            }
            None => {}
        }
        if let Some(feedback) = &effect.feedback {
            outcome.notices.push(HookNotice {
                event,
                hook_key: Some(result.hook.key.clone()),
                level: HookNoticeLevel::Feedback,
                message: bounded_hook_context_text(feedback),
            });
        }
        if let Some(context) = &effect.additional_context {
            outcome.additional_context.push(context.clone());
        }
        if let Some(updated_input) = &effect.updated_input {
            if accepts_updated_input(event, effect) {
                outcome.updated_input = Some(updated_input.clone());
            }
        }
    }
    if outcome.blocking_reason.is_some() || outcome.stop_reason.is_some() {
        outcome.allowed = false;
        outcome.updated_input = None;
    }
    outcome
}

pub fn hook_context_messages(event: HookEventName, outcome: &HookOutcome) -> Vec<String> {
    let mut messages = Vec::new();
    let mut total = 0usize;
    for context in &outcome.additional_context {
        push_bounded_context(
            &mut messages,
            &mut total,
            format!(
                "{} hook additional context: {}",
                event.as_str(),
                bounded_hook_context_text(context)
            ),
        );
    }
    for notice in &outcome.notices {
        if notice.level == HookNoticeLevel::Feedback {
            push_bounded_context(
                &mut messages,
                &mut total,
                format!(
                    "{} hook feedback: {}",
                    notice.event.as_str(),
                    bounded_hook_context_text(&notice.message)
                ),
            );
        }
    }
    messages
}

pub fn render_hook_context_block(contexts: Vec<String>) -> Option<String> {
    let mut unique = Vec::<String>::new();
    let mut total = 0usize;
    for context in contexts {
        let context = context.trim();
        if context.is_empty() || unique.iter().any(|existing| existing == context) {
            continue;
        }
        if total >= MAX_HOOK_CONTEXT_TOTAL_CHARS {
            break;
        }
        let remaining = MAX_HOOK_CONTEXT_TOTAL_CHARS - total;
        let bounded = truncate_chars(context, remaining);
        total += bounded.chars().count();
        unique.push(bounded);
    }
    if unique.is_empty() {
        return None;
    }

    let mut content = String::from("Additional context from hooks:\n");
    for context in unique {
        content.push_str("- ");
        content.push_str(&context);
        content.push('\n');
    }
    Some(content)
}

pub fn bounded_hook_context_text(text: &str) -> String {
    let redacted = crate::turn_trace::redact_string(text.trim());
    let framed = single_line_hook_text(&redacted);
    truncate_chars(&framed, MAX_HOOK_CONTEXT_ITEM_CHARS)
}

fn single_line_hook_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push(' '),
            ch if ch.is_control() => out.push(' '),
            ch => out.push(ch),
        }
    }
    out
}

fn push_bounded_context(messages: &mut Vec<String>, total: &mut usize, message: String) {
    if *total >= MAX_HOOK_CONTEXT_TOTAL_CHARS {
        return;
    }
    let remaining = MAX_HOOK_CONTEXT_TOTAL_CHARS - *total;
    let bounded = truncate_chars(&message, remaining);
    *total += bounded.chars().count();
    messages.push(bounded);
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub("...[truncated]".len());
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...[truncated]");
    out
}

fn effective_decision(event: HookEventName, result: &HookDispatchResult) -> Option<HookDecision> {
    match result.run.effect.decision {
        Some(HookDecision::Block | HookDecision::Deny | HookDecision::Stop) => {
            result.run.effect.decision
        }
        _ if fail_closed_reason(event, result).is_some() => Some(HookDecision::Block),
        other => other,
    }
}

fn decision_reason(
    event: HookEventName,
    result: &HookDispatchResult,
    decision: Option<HookDecision>,
) -> Option<String> {
    if decision == Some(HookDecision::Block)
        && result.run.effect.decision != Some(HookDecision::Block)
    {
        if let Some(reason) = fail_closed_reason(event, result) {
            return Some(reason);
        }
    }
    result.run.effect.reason.clone()
}

fn fail_closed_reason(event: HookEventName, result: &HookDispatchResult) -> Option<String> {
    if !matches!(
        event,
        HookEventName::PreToolUse | HookEventName::PermissionRequest
    ) {
        return None;
    }
    result.run.fail_closed_reason()
}

fn accepts_updated_input(event: HookEventName, effect: &HookEffect) -> bool {
    match event {
        HookEventName::PreToolUse => effect.decision == Some(HookDecision::Allow),
        HookEventName::UserPromptSubmit => !matches!(
            effect.decision,
            Some(HookDecision::Block | HookDecision::Deny | HookDecision::Stop)
        ),
        _ => false,
    }
}

pub fn plan_updated_payload_from_tool_result(
    ctx: &HookInvocationContext,
    tool_use_id: &str,
    output: &str,
) -> Option<Value> {
    let value: Value = serde_json::from_str(output).ok()?;
    if value.get("plan_updated").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let steps = value.get("steps").and_then(Value::as_array)?.clone();
    let done = value
        .get("done")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| count_steps_with_status(&steps, "done") as u64);
    let total = value
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or(steps.len() as u64);
    let active_step = steps
        .iter()
        .find(|step| step.get("status").and_then(Value::as_str) == Some("in_progress"))
        .cloned();

    let mut payload = ctx
        .payload(HookEventName::PlanUpdated)
        .insert("tool_use_id", json!(tool_use_id))
        .insert("progress", json!({ "done": done, "total": total }))
        .insert("plan", Value::Array(steps));
    if let Some(active_step) = active_step {
        payload = payload.insert("active_step", active_step);
    }
    Some(payload.into_value())
}

fn count_steps_with_status(steps: &[Value], status: &str) -> usize {
    steps
        .iter()
        .filter(|step| step.get("status").and_then(Value::as_str) == Some(status))
        .count()
}

fn hook_decision_name(decision: HookDecision) -> &'static str {
    match decision {
        HookDecision::Block => "block",
        HookDecision::Allow => "allow",
        HookDecision::Deny => "deny",
        HookDecision::Stop => "stop",
    }
}

fn append_trace(trace: &SharedTurnTrace, payload: TracePayload) {
    if let Ok(guard) = trace.lock() {
        let _ = guard.append(payload);
    }
}

fn trace_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut preview: String = trimmed.chars().take(400).collect();
    if trimmed.chars().count() > 400 {
        preview.push_str("...[truncated]");
    }
    Some(crate::turn_trace::redact_string(&preview))
}

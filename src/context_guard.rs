use anyhow::Result;
use reqwest::Client;
use std::path::PathBuf;

use crate::backends::BackendDescriptor;
use crate::budget::{format_bytes, headroom_bytes, measure_prompt_budget, usage_ratio, PromptBudget};
use crate::config::{AgentConfig, ContextConfig};
use crate::openai::{stream_chat, ChatMessage, ChatRequest, StreamOptions, ToolDef};

const SUMMARIZE_BUDGET_FRACTION: f64 = 0.5;
const TIER2_TOOL_MAX_CHARS: usize = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactMethod {
    None,
    LlmSummary,
    DeterministicTrim,
}

#[derive(Debug, Clone)]
pub struct CompactResult {
    pub compacted: bool,
    pub before_messages: usize,
    pub after_messages: usize,
    pub before_ratio: f64,
    pub after_ratio: f64,
    pub method: CompactMethod,
}

#[derive(Debug, Clone)]
pub struct ContextGuardConfig {
    pub effective_limit_bytes: usize,
    pub compact_threshold: f64,
    pub auto_compact: bool,
    pub keep_messages: usize,
    pub model_context_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct ContextGuardParams {
    pub effective_limit_bytes: usize,
    pub compact_threshold: f64,
    pub auto_compact: bool,
    pub keep_messages: usize,
    pub summarize_budget_bytes: usize,
}

pub fn default_model_context_tokens(model: &str, is_local: bool) -> usize {
    let lower = model.to_ascii_lowercase();
    if !is_local {
        return 128_000;
    }
    if lower.contains("qwen2.5-coder") || lower.contains("qwen2.5_coder") {
        return 32_768;
    }
    if lower.contains("32k") {
        return 32_768;
    }
    if lower.contains("16k") {
        return 16_384;
    }
    if lower.contains("8k") {
        return 8192;
    }
    8192
}

pub fn resolve_model_context_tokens(config: &ContextConfig, model: &str, is_local: bool) -> usize {
    config
        .model_context_tokens
        .unwrap_or_else(|| default_model_context_tokens(model, is_local))
}

pub fn resolve_auto_compact(config: &ContextConfig, is_local: bool) -> bool {
    config.auto_compact.unwrap_or(is_local)
}

pub fn effective_limit_bytes(config: &ContextConfig, model: &str, is_local: bool) -> usize {
    let model_tokens = resolve_model_context_tokens(config, model, is_local);
    let fill_ratio = 1.0 - config.reserve_ratio.clamp(0.05, 0.5);
    let from_model = (model_tokens as f64 * 4.0 * fill_ratio) as usize;
    match config.max_bytes {
        Some(max_bytes) => from_model.min(max_bytes),
        None => from_model,
    }
}

pub fn guard_config_from(config: &AgentConfig, model: &str, is_local: bool) -> ContextGuardConfig {
    ContextGuardConfig {
        effective_limit_bytes: effective_limit_bytes(&config.context, model, is_local),
        compact_threshold: config.context.compact_threshold,
        auto_compact: resolve_auto_compact(&config.context, is_local),
        keep_messages: config.context.max_messages.unwrap_or(12).clamp(4, 80),
        model_context_tokens: resolve_model_context_tokens(&config.context, model, is_local),
    }
}

pub fn guard_params_from(
    config: &AgentConfig,
    model: &str,
    is_local: bool,
) -> ContextGuardParams {
    let guard = guard_config_from(config, model, is_local);
    ContextGuardParams {
        effective_limit_bytes: guard.effective_limit_bytes,
        compact_threshold: guard.compact_threshold,
        auto_compact: guard.auto_compact,
        keep_messages: guard.keep_messages,
        summarize_budget_bytes: (guard.effective_limit_bytes as f64 * SUMMARIZE_BUDGET_FRACTION)
            as usize,
    }
}

pub fn should_compact(budget: &PromptBudget, limit_bytes: usize, threshold: f64) -> bool {
    if limit_bytes == 0 {
        return false;
    }
    usage_ratio(budget, limit_bytes) >= threshold.clamp(0.5, 0.99)
}

pub fn format_usage_line(budget: &PromptBudget, limit_bytes: usize) -> String {
    format!(
        "{} ({:.0}% of {})",
        format_bytes(budget.effective_total_bytes),
        usage_ratio(budget, limit_bytes) * 100.0,
        format_bytes(limit_bytes)
    )
}

fn system_content(messages: &[ChatMessage], fallback: &str) -> String {
    messages
        .first()
        .and_then(|m| match m {
            ChatMessage::System { content } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn non_system_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .filter(|m| !matches!(m, ChatMessage::System { .. }))
        .cloned()
        .collect()
}

fn transcript_json_bytes(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| serde_json::to_vec(m).map(|v| v.len()).unwrap_or(0))
        .sum()
}

fn shrink_tool_messages(messages: &mut [ChatMessage]) {
    for message in messages.iter_mut() {
        if let ChatMessage::Tool { content, .. } = message {
            if content.chars().count() > TIER2_TOOL_MAX_CHARS {
                let trimmed: String = content.chars().take(TIER2_TOOL_MAX_CHARS).collect();
                *content = format!("{trimmed}…[trimmed for context]");
            }
        }
    }
}

fn deterministic_trim(
    messages: &[ChatMessage],
    keep: usize,
    system_base: &str,
) -> Vec<ChatMessage> {
    let non_system = non_system_messages(messages);
    if non_system.len() <= keep {
        return messages.to_vec();
    }
    let split_at = non_system.len().saturating_sub(keep);
    let mut recent: Vec<ChatMessage> = non_system[split_at..].to_vec();
    shrink_tool_messages(&mut recent);
    let mut out = vec![ChatMessage::System {
        content: format!(
            "{system_base}\n\nConversation summary:\n[Earlier conversation trimmed to fit context budget]"
        ),
    }];
    out.extend(recent);
    out
}

pub async fn summarize_transcript(
    http: &Client,
    backend: &BackendDescriptor,
    model: &str,
    older: &[ChatMessage],
) -> Result<String> {
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
        model,
        messages: &messages,
        tools: None,
        stream: true,
        stream_options: Some(StreamOptions {
            include_usage: false,
        }),
        max_tokens: None,
    };
    let mut out = String::new();
    stream_chat(http, backend, &req, None, |chunk| {
        if let Some(choice) = chunk.choices.first() {
            if let Some(content) = &choice.delta.content {
                out.push_str(content);
            }
        }
    })
    .await?;
    Ok(out)
}

fn compact_notice(
    before_messages: usize,
    after_messages: usize,
    before_ratio: f64,
    after_ratio: f64,
    method: CompactMethod,
) -> String {
    let method_label = match method {
        CompactMethod::LlmSummary => "summarized",
        CompactMethod::DeterministicTrim => "trimmed",
        CompactMethod::None => "compacted",
    };
    format!(
        "Compacted {before_messages} messages → {after_messages} ({method_label}), budget {:.0}% → {:.0}%",
        before_ratio * 100.0,
        after_ratio * 100.0
    )
}

async fn compact_messages_core(
    messages: &mut Vec<ChatMessage>,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    keep: usize,
    limit_bytes: usize,
    summarize_budget_bytes: usize,
    http: &Client,
    backend: &BackendDescriptor,
    model: &str,
    _force: bool,
) -> Result<CompactResult> {
    let budget_before = measure_prompt_budget(system_prompt, messages, tool_defs);
    let before_messages = messages.len();
    let before_ratio = usage_ratio(&budget_before, limit_bytes);

    if before_messages <= keep + 1 {
        return Ok(CompactResult {
            compacted: false,
            before_messages,
            after_messages: before_messages,
            before_ratio,
            after_ratio: before_ratio,
            method: CompactMethod::None,
        });
    }

    let system_base = system_content(messages, system_prompt);
    let non_system = non_system_messages(messages);
    if non_system.len() <= keep {
        return Ok(CompactResult {
            compacted: false,
            before_messages,
            after_messages: before_messages,
            before_ratio,
            after_ratio: before_ratio,
            method: CompactMethod::None,
        });
    }

    let split_at = non_system.len().saturating_sub(keep);
    let older = non_system[..split_at].to_vec();
    let recent: Vec<ChatMessage> = non_system[split_at..].to_vec();

    let use_tier2 = transcript_json_bytes(&older) > summarize_budget_bytes;
    let compacted_messages = if use_tier2 {
        deterministic_trim(messages, keep, &system_base)
    } else {
        let summary = summarize_transcript(http, backend, model, &older).await?;
        let mut out = vec![ChatMessage::System {
            content: format!(
                "{system_base}\n\nConversation summary:\n{}",
                summary.trim()
            ),
        }];
        out.extend(recent);
        out
    };

    *messages = compacted_messages;
    let budget_after = measure_prompt_budget(system_prompt, messages, tool_defs);
    let after_ratio = usage_ratio(&budget_after, limit_bytes);
    let method = if use_tier2 {
        CompactMethod::DeterministicTrim
    } else {
        CompactMethod::LlmSummary
    };

    Ok(CompactResult {
        compacted: true,
        before_messages,
        after_messages: messages.len(),
        before_ratio,
        after_ratio,
        method,
    })
}

pub async fn compact_messages(
    messages: &mut Vec<ChatMessage>,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    config: &AgentConfig,
    model: &str,
    is_local: bool,
    http: &Client,
    backend: &BackendDescriptor,
    keep: Option<usize>,
    force: bool,
) -> Result<CompactResult> {
    let guard = guard_config_from(config, model, is_local);
    let keep = keep.unwrap_or(guard.keep_messages);
    let summarize_budget = (guard.effective_limit_bytes as f64 * SUMMARIZE_BUDGET_FRACTION) as usize;

    compact_messages_core(
        messages,
        system_prompt,
        tool_defs,
        keep,
        guard.effective_limit_bytes,
        summarize_budget,
        http,
        backend,
        model,
        force,
    )
    .await
}

pub async fn maybe_auto_compact_messages(
    messages: &mut Vec<ChatMessage>,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    config: &AgentConfig,
    model: &str,
    is_local: bool,
    http: &Client,
    backend: &BackendDescriptor,
) -> Result<Option<String>> {
    let guard = guard_config_from(config, model, is_local);
    if !guard.auto_compact {
        let budget = measure_prompt_budget(system_prompt, messages, tool_defs);
        if should_compact(&budget, guard.effective_limit_bytes, guard.compact_threshold) {
            return Ok(Some(format!(
                "  \x1b[33m!\x1b[0m \x1b[2mprompt budget is {} — run /compact or enable autoCompact\x1b[0m",
                format_usage_line(&budget, guard.effective_limit_bytes)
            )));
        }
        return Ok(None);
    }

    let budget = measure_prompt_budget(system_prompt, messages, tool_defs);
    if !should_compact(&budget, guard.effective_limit_bytes, guard.compact_threshold) {
        return Ok(None);
    }

    let summarize_budget = (guard.effective_limit_bytes as f64 * SUMMARIZE_BUDGET_FRACTION) as usize;
    let result = compact_messages_core(
        messages,
        system_prompt,
        tool_defs,
        guard.keep_messages,
        guard.effective_limit_bytes,
        summarize_budget,
        http,
        backend,
        model,
        false,
    )
    .await?;

    if !result.compacted {
        return Ok(None);
    }

    Ok(Some(format!(
        "  \x1b[32m✓\x1b[0m \x1b[2m{}\x1b[0m",
        compact_notice(
            result.before_messages,
            result.after_messages,
            result.before_ratio,
            result.after_ratio,
            result.method
        )
    )))
}

pub fn new_session_after_compact(session_dir: &str, session_path: &mut PathBuf) {
    *session_path = crate::session::new_session_path(session_dir);
}

pub async fn compact_session(
    messages: &mut Vec<ChatMessage>,
    session_dir: &str,
    session_path: &mut PathBuf,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    config: &AgentConfig,
    model: &str,
    is_local: bool,
    http: &Client,
    backend: &BackendDescriptor,
    keep: Option<usize>,
    force: bool,
) -> Result<CompactResult> {
    let result = compact_messages(
        messages,
        system_prompt,
        tool_defs,
        config,
        model,
        is_local,
        http,
        backend,
        keep,
        force,
    )
    .await?;

    if result.compacted {
        new_session_after_compact(session_dir, session_path);
        for message in messages.iter() {
            let _ = crate::session::save_message(session_path, message);
        }
    }

    Ok(result)
}

pub async fn maybe_auto_compact(
    messages: &mut Vec<ChatMessage>,
    session_dir: &str,
    session_path: &mut PathBuf,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    config: &AgentConfig,
    model: &str,
    is_local: bool,
    http: &Client,
    backend: &BackendDescriptor,
) -> Result<Option<String>> {
    let guard = guard_config_from(config, model, is_local);
    if !guard.auto_compact {
        let budget = measure_prompt_budget(system_prompt, messages, tool_defs);
        if should_compact(&budget, guard.effective_limit_bytes, guard.compact_threshold) {
            return Ok(Some(format!(
                "  \x1b[33m!\x1b[0m \x1b[2mprompt budget is {} — run /compact or enable autoCompact\x1b[0m",
                format_usage_line(&budget, guard.effective_limit_bytes)
            )));
        }
        return Ok(None);
    }

    let notice = maybe_auto_compact_messages(
        messages,
        system_prompt,
        tool_defs,
        config,
        model,
        is_local,
        http,
        backend,
    )
    .await?;

    if notice.is_some() {
        new_session_after_compact(session_dir, session_path);
        for message in messages.iter() {
            let _ = crate::session::save_message(session_path, message);
        }
    }

    Ok(notice)
}

pub async fn maybe_compact_messages(
    messages: &mut Vec<ChatMessage>,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    guard: &ContextGuardParams,
    http: &Client,
    backend: &BackendDescriptor,
    model: &str,
) -> Result<Option<String>> {
    if !guard.auto_compact {
        return Ok(None);
    }

    let budget = measure_prompt_budget(system_prompt, messages, tool_defs);
    if !should_compact(&budget, guard.effective_limit_bytes, guard.compact_threshold) {
        return Ok(None);
    }

    let result = compact_messages_core(
        messages,
        system_prompt,
        tool_defs,
        guard.keep_messages,
        guard.effective_limit_bytes,
        guard.summarize_budget_bytes,
        http,
        backend,
        model,
        false,
    )
    .await?;

    if !result.compacted {
        return Ok(None);
    }

    Ok(Some(format!(
        "  \x1b[32m✓\x1b[0m \x1b[2m{}\x1b[0m",
        compact_notice(
            result.before_messages,
            result.after_messages,
            result.before_ratio,
            result.after_ratio,
            result.method
        )
    )))
}

pub fn context_status_lines(
    config: &AgentConfig,
    model: &str,
    is_local: bool,
    budget: &PromptBudget,
    last_notice: Option<&str>,
) -> Vec<String> {
    let guard = guard_config_from(config, model, is_local);
    let ratio = usage_ratio(budget, guard.effective_limit_bytes);
    let mut lines = vec![
        format!(
            "  \x1b[2meffectiveLimit\x1b[0m  {} (~{} model tokens, {:.0}% used, {} headroom)",
            format_bytes(guard.effective_limit_bytes),
            guard.model_context_tokens,
            ratio * 100.0,
            format_bytes(headroom_bytes(budget, guard.effective_limit_bytes))
        ),
        format!(
            "  \x1b[2mautoGuard\x1b[0m     autoCompact={} threshold={:.0}% reserve={:.0}%",
            guard.auto_compact,
            guard.compact_threshold * 100.0,
            config.context.reserve_ratio * 100.0
        ),
    ];
    if let Some(notice) = last_notice {
        lines.push(format!("  \x1b[2mlastGuard\x1b[0m     {notice}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ContextConfig;

    #[test]
    fn effective_limit_uses_model_tokens_and_max_bytes_min() {
        let mut config = ContextConfig::default();
        config.model_context_tokens = Some(8192);
        config.max_bytes = Some(256 * 1024);
        config.reserve_ratio = 0.25;
        let limit = effective_limit_bytes(&config, "qwen2.5-coder:7b", true);
        assert_eq!(limit, 8192 * 4 * 3 / 4);
    }

    #[test]
    fn effective_limit_respects_max_bytes_cap() {
        let mut config = ContextConfig::default();
        config.model_context_tokens = Some(32768);
        config.max_bytes = Some(32 * 1024);
        let limit = effective_limit_bytes(&config, "big-model", true);
        assert_eq!(limit, 32 * 1024);
    }

    #[test]
    fn should_compact_at_threshold_not_before() {
        let budget = PromptBudget {
            system_bytes: 0,
            transcript_bytes: 0,
            tool_schema_bytes: 0,
            tool_result_bytes: 0,
            total_bytes: 840,
            effective_total_bytes: 840,
            estimated_tokens: 210,
        };
        assert!(!should_compact(&budget, 1000, 0.85));
        let budget_high = PromptBudget {
            effective_total_bytes: 850,
            total_bytes: 850,
            ..budget
        };
        assert!(should_compact(&budget_high, 1000, 0.85));
    }

    #[test]
    fn auto_compact_defaults_local_on_cloud_off() {
        let config = ContextConfig::default();
        assert!(resolve_auto_compact(&config, true));
        assert!(!resolve_auto_compact(&config, false));
    }

    #[test]
    fn deterministic_trim_drops_older_messages() {
        let messages = vec![
            ChatMessage::System {
                content: "sys".into(),
            },
            ChatMessage::User {
                content: "one".into(),
            },
            ChatMessage::Assistant {
                content: Some("two".into()),
                tool_calls: vec![],
            },
            ChatMessage::User {
                content: "three".into(),
            },
        ];
        let trimmed = deterministic_trim(&messages, 1, "sys");
        assert_eq!(trimmed.len(), 2);
        assert!(matches!(trimmed[0], ChatMessage::System { .. }));
        assert!(matches!(
            &trimmed[1],
            ChatMessage::User { content } if content == "three"
        ));
    }

    #[test]
    fn default_model_tokens_conservative_for_unknown_local() {
        assert_eq!(default_model_context_tokens("my-model", true), 8192);
        assert_eq!(
            default_model_context_tokens("qwen2.5-coder:7b", true),
            32768
        );
        assert_eq!(default_model_context_tokens("gpt-4", false), 128_000);
    }
}

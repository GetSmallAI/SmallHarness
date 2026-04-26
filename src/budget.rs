use serde::Serialize;

use crate::openai::{ChatMessage, ToolDef};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptBudget {
    pub system_bytes: usize,
    pub transcript_bytes: usize,
    pub tool_schema_bytes: usize,
    pub tool_result_bytes: usize,
    pub total_bytes: usize,
    pub estimated_tokens: usize,
}

pub fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

pub fn measure_prompt_budget(
    system_prompt: &str,
    messages: &[ChatMessage],
    tool_defs: &[ToolDef],
) -> PromptBudget {
    let system_bytes = system_prompt.len();
    let transcript_bytes = messages
        .iter()
        .filter(|m| !matches!(m, ChatMessage::System { .. }))
        .map(|m| serde_json::to_vec(m).map(|v| v.len()).unwrap_or(0))
        .sum();
    let tool_result_bytes = messages
        .iter()
        .map(|m| match m {
            ChatMessage::Tool { content, .. } => content.len(),
            _ => 0,
        })
        .sum();
    let tool_schema_bytes = if tool_defs.is_empty() {
        0
    } else {
        serde_json::to_vec(tool_defs).map(|v| v.len()).unwrap_or(0)
    };
    let total_bytes = system_bytes + transcript_bytes + tool_schema_bytes;
    PromptBudget {
        system_bytes,
        transcript_bytes,
        tool_schema_bytes,
        tool_result_bytes,
        total_bytes,
        estimated_tokens: estimate_tokens(total_bytes),
    }
}

pub fn format_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_tokens_at_four_bytes_each() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(8), 2);
    }

    #[test]
    fn separates_tool_result_bytes() {
        let messages = vec![ChatMessage::Tool {
            tool_call_id: "x".into(),
            content: "hello".into(),
        }];
        let budget = measure_prompt_budget("system", &messages, &[]);
        assert_eq!(budget.system_bytes, 6);
        assert_eq!(budget.tool_result_bytes, 5);
        assert!(budget.transcript_bytes >= 5);
    }
}

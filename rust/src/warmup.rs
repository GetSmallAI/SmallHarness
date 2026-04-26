use anyhow::Result;
use std::time::Instant;

use crate::backends::BackendDescriptor;
use crate::openai::{chat_oneshot, ChatMessage, ChatRequest, ToolDef};

pub async fn warmup(
    http: &reqwest::Client,
    backend: &BackendDescriptor,
    model: &str,
    system_prompt: &str,
    tools: &[ToolDef],
) -> Result<u128> {
    let start = Instant::now();
    let messages = vec![
        ChatMessage::System {
            content: system_prompt.to_string(),
        },
        ChatMessage::User {
            content: "ok".into(),
        },
    ];
    let req = ChatRequest {
        model,
        messages: &messages,
        tools: if tools.is_empty() { None } else { Some(tools) },
        stream: false,
        stream_options: None,
        max_tokens: Some(1),
    };
    chat_oneshot(http, backend, &req).await?;
    Ok(start.elapsed().as_millis())
}

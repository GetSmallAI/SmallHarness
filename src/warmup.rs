use anyhow::Result;
use std::time::Instant;

use crate::backends::BackendDescriptor;
use crate::model_system::EffortLevel;
use crate::openai::{chat_oneshot, ChatMessage, ChatRequest, ToolDef};

pub async fn warmup(
    http: &reqwest::Client,
    backend: &BackendDescriptor,
    model: &str,
    effort: Option<EffortLevel>,
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
    let cache_key = crate::openai::session_cache_key(backend, model, &messages);
    let req = ChatRequest {
        model,
        messages: &messages,
        tools: if tools.is_empty() { None } else { Some(tools) },
        stream: false,
        stream_options: None,
        max_tokens: Some(1),
        // Use the same routing key as the real turn; otherwise a successful
        // warmup may populate a different OpenAI cache shard.
        prompt_cache_key: cache_key.as_deref(),
        effort,
    };
    chat_oneshot(http, backend, &req).await?;
    Ok(start.elapsed().as_millis())
}

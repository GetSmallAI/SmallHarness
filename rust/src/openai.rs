use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::backends::BackendDescriptor;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl ChatMessage {
    pub fn user_text(&self) -> Option<&str> {
        match self {
            ChatMessage::User { content } => Some(content),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: ToolDefFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [ToolDef]>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    #[serde(default)]
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    #[serde(default)]
    pub delta: StreamDelta,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct StreamDelta {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallDelta {
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ToolFunctionDelta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolFunctionDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("failed to build HTTP client")
}

pub async fn list_models(
    client: &reqwest::Client,
    backend: &BackendDescriptor,
) -> Result<Vec<String>> {
    let url = format!("{}/models", backend.base_url.trim_end_matches('/'));
    let resp = client
        .get(url)
        .bearer_auth(&backend.api_key)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {}", resp.status()));
    }
    #[derive(Deserialize)]
    struct ModelsResp {
        data: Vec<Model>,
    }
    #[derive(Deserialize)]
    struct Model {
        id: String,
    }
    let parsed: ModelsResp = resp.json().await?;
    Ok(parsed.data.into_iter().map(|m| m.id).collect())
}

pub async fn chat_oneshot(
    client: &reqwest::Client,
    backend: &BackendDescriptor,
    req: &ChatRequest<'_>,
) -> Result<()> {
    let url = format!("{}/chat/completions", backend.base_url.trim_end_matches('/'));
    let resp = client
        .post(url)
        .bearer_auth(&backend.api_key)
        .json(req)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {}: {}", status, body.trim()));
    }
    Ok(())
}

pub async fn stream_chat<F>(
    client: &reqwest::Client,
    backend: &BackendDescriptor,
    req: &ChatRequest<'_>,
    mut on_chunk: F,
) -> Result<()>
where
    F: FnMut(StreamChunk),
{
    let url = format!("{}/chat/completions", backend.base_url.trim_end_matches('/'));
    let resp = client
        .post(url)
        .bearer_auth(&backend.api_key)
        .json(req)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {}: {}", status, body.trim()));
    }
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut data = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line_str = std::str::from_utf8(&line)
                .map_err(|e| anyhow!("non-utf8 SSE line: {e}"))?
                .trim_end_matches(['\r', '\n']);
            if line_str.is_empty() {
                if !data.is_empty() {
                    if data.trim() == "[DONE]" {
                        return Ok(());
                    }
                    if let Ok(c) = serde_json::from_str::<StreamChunk>(&data) {
                        on_chunk(c);
                    }
                    data.clear();
                }
            } else if let Some(rest) = line_str.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest);
            }
        }
    }
    if !data.is_empty() && data.trim() != "[DONE]" {
        if let Ok(c) = serde_json::from_str::<StreamChunk>(&data) {
            on_chunk(c);
        }
    }
    Ok(())
}

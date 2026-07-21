use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::backends::{BackendDescriptor, BackendName};
use crate::cancel::CancellationToken;
use crate::model_system::EffortLevel;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: UserContent,
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

/// Body of a user-role message. Serializes either as a bare string (the
/// dominant case — every text-only turn) or as an array of content parts
/// (text + image_url), matching the OpenAI Chat Completions API's
/// multi-part user message format. Using `#[serde(untagged)]` keeps the
/// wire format identical to the pre-image shape for plain text turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Parts(Vec<UserContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

impl UserContent {
    /// Extract the textual portion of the message. For Parts variants this
    /// concatenates the text parts; non-text parts (images) are replaced
    /// with a `[image]` marker so consumers that only care about text
    /// (transcripts, summaries, byte budgets) still get something sensible.
    pub fn as_text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            UserContent::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
            UserContent::Parts(parts) => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        UserContentPart::Text { text } => out.push_str(text),
                        UserContentPart::ImageUrl { .. } => out.push_str("[image]"),
                    }
                }
                std::borrow::Cow::Owned(out)
            }
        }
    }
}

impl From<String> for UserContent {
    fn from(s: String) -> Self {
        UserContent::Text(s)
    }
}

impl From<&str> for UserContent {
    fn from(s: &str) -> Self {
        UserContent::Text(s.to_string())
    }
}

impl ChatMessage {
    pub fn user_text(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            ChatMessage::User { content } => Some(content.as_text()),
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
    #[serde(skip)]
    pub effort: Option<EffortLevel>,
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
    #[serde(default, alias = "reasoning_content", alias = "thinking")]
    pub reasoning: Option<String>,
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
    #[serde(default)]
    pub cost: Option<f64>,
}

pub fn build_http_client() -> reqwest::Client {
    // Connect timeout only: a total request timeout would cut off long
    // streaming completions, but connecting to a dead backend should fail
    // fast rather than hang until the OS gives up.
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client")
}

async fn resolve_bearer(client: &reqwest::Client, backend: &BackendDescriptor) -> Result<String> {
    if matches!(backend.name, BackendName::Grok) {
        return crate::xai_oauth::access_token(client).await;
    }
    Ok(backend.api_key.clone())
}

fn chat_request_builder(
    client: &reqwest::Client,
    backend: &BackendDescriptor,
    url: String,
    bearer: &str,
    model: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    let request = client.post(url).bearer_auth(bearer).json(body);
    if matches!(backend.name, BackendName::Grok) {
        request
            .header(
                "X-XAI-Token-Auth",
                crate::xai_oauth::TOKEN_AUTH_HEADER_VALUE,
            )
            .header("x-grok-model-override", model)
    } else {
        request
    }
}

pub async fn list_models(
    client: &reqwest::Client,
    backend: &BackendDescriptor,
) -> Result<Vec<String>> {
    if matches!(backend.name, BackendName::OpenAiCodex) {
        return Ok(crate::codex_responses::codex_model_list());
    }
    if matches!(backend.name, BackendName::Grok) {
        // Static catalog (same shape as openai-codex): avoid GET /models on
        // every `/model` open and only expose agent-ready Grok ids.
        return Ok(crate::xai_oauth::grok_model_list());
    }
    let url = format!("{}/models", backend.base_url.trim_end_matches('/'));
    let resp = client.get(url).bearer_auth(&backend.api_key).send().await?;
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
    if matches!(backend.name, BackendName::OpenAiCodex) {
        return crate::codex_responses::stream_codex_responses(client, backend, req, None, |_| {})
            .await;
    }
    let url = format!(
        "{}/chat/completions",
        backend.base_url.trim_end_matches('/')
    );
    let body = request_body(backend, req)?;
    let bearer = resolve_bearer(client, backend).await?;
    let resp = chat_request_builder(client, backend, url, &bearer, req.model, &body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {}: {}", status, body.trim()));
    }
    Ok(())
}

/// One event surfaced by [`SseParser`].
#[derive(Debug)]
pub enum SseEvent {
    Chunk(StreamChunk),
    Done,
}

/// Incremental parser for OpenAI-compatible SSE chat-completion streams.
///
/// Feed it bytes as they arrive; it accumulates `data:` lines per event and
/// emits a [`StreamChunk`] for each complete event, or [`SseEvent::Done`] for
/// the `data: [DONE]` terminator. Malformed JSON inside an event is silently
/// dropped (matches the TS implementation).
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line_str = std::str::from_utf8(&line)
                .map_err(|e| anyhow!("non-utf8 SSE line: {e}"))?
                .trim_end_matches(['\r', '\n']);
            if line_str.is_empty() {
                if !self.data.is_empty() {
                    if self.data.trim() == "[DONE]" {
                        out.push(SseEvent::Done);
                    } else if let Ok(c) = serde_json::from_str::<StreamChunk>(&self.data) {
                        out.push(SseEvent::Chunk(c));
                    }
                    self.data.clear();
                }
            } else if let Some(rest) = line_str.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(rest);
            }
        }
        Ok(out)
    }

    /// Drain any trailing data left without a terminating blank line.
    pub fn finalize(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.data.is_empty() && self.data.trim() != "[DONE]" {
            if let Ok(c) = serde_json::from_str::<StreamChunk>(&self.data) {
                out.push(SseEvent::Chunk(c));
            }
        }
        self.data.clear();
        out
    }
}

pub async fn stream_chat<F>(
    client: &reqwest::Client,
    backend: &BackendDescriptor,
    req: &ChatRequest<'_>,
    cancel: Option<CancellationToken>,
    mut on_chunk: F,
) -> Result<()>
where
    F: FnMut(StreamChunk),
{
    if matches!(backend.name, BackendName::OpenAiCodex) {
        return crate::codex_responses::stream_codex_responses(
            client, backend, req, cancel, on_chunk,
        )
        .await;
    }
    let url = format!(
        "{}/chat/completions",
        backend.base_url.trim_end_matches('/')
    );
    let body = request_body(backend, req)?;
    let bearer = resolve_bearer(client, backend).await?;
    let resp = chat_request_builder(client, backend, url, &bearer, req.model, &body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {}: {}", status, body.trim()));
    }
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    loop {
        let next = if let Some(cancel) = cancel.clone() {
            tokio::select! {
                _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                chunk = stream.next() => chunk,
            }
        } else {
            stream.next().await
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk?;
        for ev in parser.feed(&chunk)? {
            match ev {
                SseEvent::Chunk(c) => on_chunk(c),
                SseEvent::Done => return Ok(()),
            }
        }
    }
    for ev in parser.finalize() {
        if let SseEvent::Chunk(c) = ev {
            on_chunk(c);
        }
    }
    Ok(())
}

fn request_body(backend: &BackendDescriptor, req: &ChatRequest<'_>) -> Result<Value> {
    let mut body = serde_json::to_value(req)?;
    if let Some(effort) = req.effort {
        apply_effort_to_request(backend, &mut body, effort)?;
    }
    if matches!(backend.name, BackendName::Openrouter) && backend.openrouter.fusion.enabled {
        let plugin = fusion_plugin_value(&backend.openrouter.fusion)?;
        body.as_object_mut()
            .ok_or_else(|| anyhow!("chat request did not serialize to an object"))?
            .insert("plugins".into(), Value::Array(vec![plugin]));
    }
    Ok(body)
}

fn apply_effort_to_request(
    backend: &BackendDescriptor,
    body: &mut Value,
    effort: EffortLevel,
) -> Result<()> {
    let Some(obj) = body.as_object_mut() else {
        return Err(anyhow!("chat request did not serialize to an object"));
    };
    match backend.name {
        BackendName::Openrouter => {
            obj.insert(
                "reasoning".into(),
                serde_json::json!({ "effort": effort.openrouter_reasoning_effort() }),
            );
        }
        BackendName::OpenAi | BackendName::Grok => {
            if let Some(value) = effort.openai_reasoning_effort() {
                obj.insert("reasoning_effort".into(), Value::String(value.into()));
            }
        }
        _ => {}
    }
    Ok(())
}

fn fusion_plugin_value(config: &crate::backends::OpenRouterFusionConfig) -> Result<Value> {
    if config.analysis_models.len() > 8 {
        return Err(anyhow!(
            "openrouter.fusion.analysisModels supports at most 8 models"
        ));
    }
    if let Some(max_tool_calls) = config.max_tool_calls {
        if !(1..=16).contains(&max_tool_calls) {
            return Err(anyhow!(
                "openrouter.fusion.maxToolCalls must be between 1 and 16"
            ));
        }
    }

    let mut plugin = Map::new();
    plugin.insert("id".into(), Value::String("fusion".into()));
    if !config.analysis_models.is_empty() {
        plugin.insert(
            "analysis_models".into(),
            Value::Array(
                config
                    .analysis_models
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(model) = &config.judge_model {
        plugin.insert("model".into(), Value::String(model.clone()));
    }
    if let Some(max_tool_calls) = config.max_tool_calls {
        plugin.insert(
            "max_tool_calls".into(),
            Value::Number(serde_json::Number::from(max_tool_calls)),
        );
    }
    Ok(Value::Object(plugin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::{
        BackendDescriptor, BackendName, OpenRouterConfig, OpenRouterFusionConfig,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn content_of(ev: &SseEvent) -> Option<&str> {
        match ev {
            SseEvent::Chunk(c) => c.choices.first()?.delta.content.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn grok_oauth_requests_use_cli_proxy_headers() {
        let backend = BackendDescriptor {
            name: BackendName::Grok,
            base_url: crate::xai_oauth::INFERENCE_BASE_URL.into(),
            api_key: String::new(),
            is_local: false,
            openrouter: OpenRouterConfig::default(),
        };
        let request = chat_request_builder(
            &reqwest::Client::new(),
            &backend,
            format!("{}/chat/completions", backend.base_url),
            "oauth-token",
            "grok-4.5",
            &serde_json::json!({"model": "grok-4.5", "stream": true}),
        )
        .build()
        .unwrap();

        assert_eq!(
            request.url().as_str(),
            "https://cli-chat-proxy.grok.com/v1/chat/completions"
        );
        assert_eq!(
            request.headers()["x-xai-token-auth"],
            crate::xai_oauth::TOKEN_AUTH_HEADER_VALUE
        );
        assert_eq!(request.headers()["x-grok-model-override"], "grok-4.5");
        assert_eq!(request.headers()["authorization"], "Bearer oauth-token");
    }

    #[test]
    fn user_content_text_serializes_as_bare_string() {
        let msg = ChatMessage::User {
            content: UserContent::Text("hello".into()),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hello");
    }

    #[test]
    fn user_content_parts_serialize_as_array() {
        let msg = ChatMessage::User {
            content: UserContent::Parts(vec![
                UserContentPart::Text {
                    text: "describe this".into(),
                },
                UserContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,AAAA".into(),
                    },
                },
            ]),
        };
        let json = serde_json::to_value(&msg).unwrap();
        let parts = json["content"].as_array().expect("content is array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "describe this");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn user_content_text_roundtrips_through_json() {
        let wire = serde_json::json!({"role": "user", "content": "hi"});
        let msg: ChatMessage = serde_json::from_value(wire).unwrap();
        match msg {
            ChatMessage::User { content } => {
                assert!(matches!(content, UserContent::Text(ref s) if s == "hi"));
                assert_eq!(content.as_text(), "hi");
            }
            _ => panic!("expected user"),
        }
    }

    #[test]
    fn user_content_parts_roundtrip_through_json() {
        let wire = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,XX"}}
            ]
        });
        let msg: ChatMessage = serde_json::from_value(wire).unwrap();
        match msg {
            ChatMessage::User { content } => {
                let text = content.as_text();
                assert!(text.contains("what is this?"));
                assert!(text.contains("[image]"));
            }
            _ => panic!("expected user"),
        }
    }

    #[test]
    fn parses_single_chunk() {
        let mut p = SseParser::new();
        let bytes = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let events = p.feed(bytes).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(content_of(&events[0]), Some("hi"));
    }

    #[test]
    fn parses_chunks_split_across_feeds() {
        let mut p = SseParser::new();
        assert!(p
            .feed(b"data: {\"choices\":[{\"delta\":")
            .unwrap()
            .is_empty());
        assert!(p.feed(b"{\"content\":\"a\"}}]}").unwrap().is_empty());
        let events = p.feed(b"\n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(content_of(&events[0]), Some("a"));
    }

    #[test]
    fn parses_multiple_chunks_in_one_feed() {
        let mut p = SseParser::new();
        let bytes = b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n\
                      data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n";
        let events = p.feed(bytes).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(content_of(&events[0]), Some("a"));
        assert_eq!(content_of(&events[1]), Some("b"));
    }

    #[test]
    fn parses_reasoning_alias_and_content() {
        let mut p = SseParser::new();
        let bytes = b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\",\"content\":\"answer\"}}]}\n\n";
        let events = p.feed(bytes).unwrap();
        match &events[0] {
            SseEvent::Chunk(c) => {
                let delta = &c.choices[0].delta;
                assert_eq!(delta.reasoning.as_deref(), Some("think"));
                assert_eq!(delta.content.as_deref(), Some("answer"));
            }
            _ => panic!("expected chunk"),
        }
    }

    #[test]
    fn emits_done_marker() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: [DONE]\n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SseEvent::Done));
    }

    #[test]
    fn ignores_other_sse_fields() {
        let mut p = SseParser::new();
        let bytes = b"event: ping\nid: 1\n\ndata: {\"choices\":[]}\n\n";
        let events = p.feed(bytes).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn handles_crlf_line_endings() {
        let mut p = SseParser::new();
        let bytes = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\r\n\r\n";
        let events = p.feed(bytes).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(content_of(&events[0]), Some("x"));
    }

    #[test]
    fn accepts_data_without_space_after_colon() {
        let mut p = SseParser::new();
        let events = p.feed(b"data:{\"choices\":[]}\n\n").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn invalid_json_is_skipped() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: not json\n\n").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parses_byte_at_a_time() {
        let mut p = SseParser::new();
        let bytes = b"data: {\"choices\":[{\"delta\":{\"content\":\"slow\"}}]}\n\n";
        let mut total = 0;
        for b in bytes {
            total += p.feed(&[*b]).unwrap().len();
        }
        assert_eq!(total, 1);
    }

    #[test]
    fn finalize_drains_unterminated_event() {
        let mut p = SseParser::new();
        // No trailing blank line — event is still in data buffer.
        let _ = p.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}\n");
        let events = p.finalize();
        assert_eq!(events.len(), 1);
        assert_eq!(content_of(&events[0]), Some("y"));
    }

    #[test]
    fn usage_chunk_carries_token_counts() {
        let mut p = SseParser::new();
        let bytes =
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7}}\n\n";
        let events = p.feed(bytes).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SseEvent::Chunk(c) => {
                let u = c.usage.as_ref().unwrap();
                assert_eq!(u.prompt_tokens, 42);
                assert_eq!(u.completion_tokens, 7);
            }
            _ => panic!("expected chunk"),
        }
    }

    #[test]
    fn usage_chunk_carries_openrouter_cost() {
        let mut p = SseParser::new();
        let bytes = b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7,\"cost\":0.0015}}\n\n";
        let events = p.feed(bytes).unwrap();
        match &events[0] {
            SseEvent::Chunk(c) => {
                let u = c.usage.as_ref().unwrap();
                assert_eq!(u.cost, Some(0.0015));
            }
            _ => panic!("expected chunk"),
        }
    }

    #[test]
    fn openrouter_fusion_plugin_is_injected_when_enabled() {
        let backend = BackendDescriptor {
            name: BackendName::Openrouter,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "test".into(),
            is_local: false,
            openrouter: OpenRouterConfig {
                fusion: OpenRouterFusionConfig {
                    enabled: true,
                    analysis_models: vec![
                        "~google/gemini-flash-latest".into(),
                        "deepseek/deepseek-v3.2".into(),
                    ],
                    judge_model: Some("~anthropic/claude-opus-latest".into()),
                    max_tool_calls: Some(4),
                },
            },
        };
        let messages = vec![ChatMessage::User {
            content: "compare approaches".into(),
        }];
        let req = ChatRequest {
            model: "anthropic/claude-sonnet-4.5",
            messages: &messages,
            tools: None,
            stream: true,
            stream_options: None,
            max_tokens: None,
            effort: None,
        };

        let body = request_body(&backend, &req).unwrap();
        let plugin = &body["plugins"][0];
        assert_eq!(plugin["id"], "fusion");
        assert_eq!(plugin["analysis_models"][0], "~google/gemini-flash-latest");
        assert_eq!(plugin["model"], "~anthropic/claude-opus-latest");
        assert_eq!(plugin["max_tool_calls"], 4);
    }

    #[test]
    fn openrouter_effort_is_injected_as_reasoning() {
        let backend = BackendDescriptor {
            name: BackendName::Openrouter,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "test".into(),
            is_local: false,
            openrouter: OpenRouterConfig::default(),
        };
        let messages = vec![ChatMessage::User {
            content: "plan a refactor".into(),
        }];
        let req = ChatRequest {
            model: "openrouter/fusion",
            messages: &messages,
            tools: None,
            stream: true,
            stream_options: None,
            max_tokens: None,
            effort: Some(EffortLevel::Max),
        };

        let body = request_body(&backend, &req).unwrap();
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert!(body.get("effort").is_none());
    }

    #[test]
    fn local_backends_ignore_effort_request_field() {
        let backend = BackendDescriptor {
            name: BackendName::Ollama,
            base_url: "http://localhost:11434/v1".into(),
            api_key: "test".into(),
            is_local: true,
            openrouter: OpenRouterConfig::default(),
        };
        let messages = vec![ChatMessage::User {
            content: "small edit".into(),
        }];
        let req = ChatRequest {
            model: "qwen2.5-coder:7b",
            messages: &messages,
            tools: None,
            stream: true,
            stream_options: None,
            max_tokens: None,
            effort: Some(EffortLevel::High),
        };

        let body = request_body(&backend, &req).unwrap();
        assert!(body.get("reasoning").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("effort").is_none());
    }

    #[test]
    fn fusion_plugin_is_not_injected_for_other_backends() {
        let backend = BackendDescriptor {
            name: BackendName::OpenAi,
            base_url: "https://api.openai.com/v1".into(),
            api_key: "test".into(),
            is_local: false,
            openrouter: OpenRouterConfig {
                fusion: OpenRouterFusionConfig {
                    enabled: true,
                    ..Default::default()
                },
            },
        };
        let messages = vec![ChatMessage::User {
            content: "hello".into(),
        }];
        let req = ChatRequest {
            model: "gpt-4o-mini",
            messages: &messages,
            tools: None,
            stream: true,
            stream_options: None,
            max_tokens: None,
            effort: None,
        };

        let body = request_body(&backend, &req).unwrap();
        assert!(body.get("plugins").is_none());
    }

    #[test]
    fn fusion_plugin_rejects_invalid_limits() {
        let too_many_models = OpenRouterFusionConfig {
            enabled: true,
            analysis_models: vec![
                "m1".into(),
                "m2".into(),
                "m3".into(),
                "m4".into(),
                "m5".into(),
                "m6".into(),
                "m7".into(),
                "m8".into(),
                "m9".into(),
            ],
            ..Default::default()
        };
        assert!(fusion_plugin_value(&too_many_models).is_err());

        let bad_max_tools = OpenRouterFusionConfig {
            enabled: true,
            max_tool_calls: Some(17),
            ..Default::default()
        };
        assert!(fusion_plugin_value(&bad_max_tools).is_err());
    }

    #[tokio::test]
    async fn stream_chat_reads_mock_sse_tool_call() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"file_read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"src/main.rs\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let backend = BackendDescriptor {
            name: BackendName::Ollama,
            base_url: format!("http://{addr}/v1"),
            api_key: "test".into(),
            is_local: true,
            openrouter: OpenRouterConfig::default(),
        };
        let messages = vec![ChatMessage::User {
            content: "read main".into(),
        }];
        let req = ChatRequest {
            model: "mock",
            messages: &messages,
            tools: None,
            stream: true,
            stream_options: None,
            max_tokens: None,
            effort: None,
        };
        let mut name = String::new();
        let mut args = String::new();
        stream_chat(&reqwest::Client::new(), &backend, &req, None, |chunk| {
            if let Some(call) = chunk
                .choices
                .first()
                .and_then(|choice| choice.delta.tool_calls.as_ref())
                .and_then(|calls| calls.first())
            {
                if let Some(function) = &call.function {
                    if let Some(n) = &function.name {
                        name.push_str(n);
                    }
                    if let Some(a) = &function.arguments {
                        args.push_str(a);
                    }
                }
            }
        })
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(name, "file_read");
        assert_eq!(args, "{\"path\":\"src/main.rs\"}");
    }
}

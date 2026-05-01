use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::backends::BackendDescriptor;
use crate::cancel::CancellationToken;

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
    let url = format!(
        "{}/chat/completions",
        backend.base_url.trim_end_matches('/')
    );
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
    let url = format!(
        "{}/chat/completions",
        backend.base_url.trim_end_matches('/')
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::{BackendDescriptor, BackendName};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn content_of(ev: &SseEvent) -> Option<&str> {
        match ev {
            SseEvent::Chunk(c) => c.choices.first()?.delta.content.as_deref(),
            _ => None,
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

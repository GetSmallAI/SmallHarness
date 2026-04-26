use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backends::BackendDescriptor;
use crate::cancel::CancellationToken;
use crate::openai::{
    stream_chat, ChatMessage, ChatRequest, StreamOptions, ToolCall, ToolDef, ToolDefFunction,
    ToolFunction,
};
use crate::tools::{Tool, ToolPreview};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Text {
        delta: String,
    },
    ToolCall {
        name: String,
        call_id: String,
        args: Value,
    },
    ToolResult {
        name: String,
        call_id: String,
        output: String,
    },
    #[allow(dead_code)]
    Reasoning {
        delta: String,
    },
}

#[async_trait]
pub trait ApprovalProvider: Send {
    async fn approve(&mut self, name: &str, args: &Value, preview: Option<&ToolPreview>) -> bool;
}

pub struct RunResult {
    pub messages: Vec<ChatMessage>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

pub fn to_openai_tools(tools: &[Arc<dyn Tool>]) -> Vec<ToolDef> {
    tools
        .iter()
        .map(|t| ToolDef {
            kind: "function",
            function: ToolDefFunction {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.input_schema(),
            },
        })
        .collect()
}

fn looks_like_start_of_tool_call(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"^\s*(?:```(?:json)?\s*)?\{\s*"?name"?\s*:"#).expect("looks_like regex")
    });
    re.is_match(text)
}

fn try_parse_inline_tool_call(text: &str, tool_names: &HashSet<String>) -> Option<(String, Value)> {
    static OPEN: OnceLock<Regex> = OnceLock::new();
    static CLOSE: OnceLock<Regex> = OnceLock::new();
    let open = OPEN.get_or_init(|| Regex::new(r"^```(?:json)?\s*").unwrap());
    let close = CLOSE.get_or_init(|| Regex::new(r"\s*```$").unwrap());
    let trimmed = text.trim();
    let stripped = open.replace(trimmed, "");
    let stripped = close.replace(&stripped, "");
    let t = stripped.trim();
    if !t.starts_with('{') {
        return None;
    }
    let parsed: Value = serde_json::from_str(t).ok()?;
    let name = parsed.get("name")?.as_str()?.to_string();
    if !tool_names.contains(&name) {
        return None;
    }
    let args = parsed
        .get("arguments")
        .or_else(|| parsed.get("parameters"))
        .or_else(|| parsed.get("args"))
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if !args.is_object() {
        return None;
    }
    Some((name, args))
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("…[truncated]");
    out
}

fn compact_tool_output(name: &str, output: &str) -> String {
    const MAX_TOOL_OUTPUT_CHARS: usize = 4000;
    if output.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return output.to_string();
    }
    if let Ok(mut parsed) = serde_json::from_str::<Value>(output) {
        if let Some(obj) = parsed.as_object_mut() {
            for key in ["content", "output", "diff"] {
                if let Some(Value::String(s)) = obj.get_mut(key) {
                    *s = truncate_chars(s, MAX_TOOL_OUTPUT_CHARS);
                    obj.insert("compacted".into(), Value::Bool(true));
                    obj.insert(
                        "summary".into(),
                        Value::String(format!("{name} output compacted for model context")),
                    );
                    return serde_json::to_string(&parsed)
                        .unwrap_or_else(|_| truncate_chars(output, MAX_TOOL_OUTPUT_CHARS));
                }
            }
            for key in ["matches", "entries"] {
                if let Some(Value::Array(items)) = obj.get_mut(key) {
                    let original = items.len();
                    items.truncate(50);
                    let kept = items.len();
                    obj.insert("compacted".into(), Value::Bool(true));
                    obj.insert(
                        "summary".into(),
                        Value::String(format!(
                            "{name} returned {original} items; kept first {}",
                            kept
                        )),
                    );
                    return serde_json::to_string(&parsed)
                        .unwrap_or_else(|_| truncate_chars(output, MAX_TOOL_OUTPUT_CHARS));
                }
            }
        }
    }
    truncate_chars(output, MAX_TOOL_OUTPUT_CHARS)
}

#[allow(clippy::too_many_arguments)]
pub async fn run_agent<F>(
    http: &reqwest::Client,
    backend: &BackendDescriptor,
    model: &str,
    initial_messages: Vec<ChatMessage>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    mut on_event: F,
    mut approve: Option<&mut dyn ApprovalProvider>,
    cancel: Option<CancellationToken>,
) -> Result<RunResult>
where
    F: FnMut(AgentEvent),
{
    let mut messages = initial_messages;
    let tool_defs = to_openai_tools(&tools);
    let tool_map: HashMap<String, Arc<dyn Tool>> = tools
        .iter()
        .map(|t| (t.name().to_string(), t.clone()))
        .collect();
    let tool_names: HashSet<String> = tools.iter().map(|t| t.name().to_string()).collect();

    let mut total_in: u32 = 0;
    let mut total_out: u32 = 0;

    for step in 0..max_steps {
        if cancel.as_ref().map(|c| c.is_cancelled()).unwrap_or(false) {
            break;
        }
        let req = ChatRequest {
            model,
            messages: &messages,
            tools: if tool_defs.is_empty() {
                None
            } else {
                Some(&tool_defs)
            },
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            max_tokens: None,
        };

        let mut assistant_text = String::new();
        let mut buffering_inline = false;
        let mut tool_calls: BTreeMap<usize, (String, String, String)> = BTreeMap::new();

        stream_chat(http, backend, &req, cancel.clone(), |chunk| {
            if let Some(choice) = chunk.choices.first() {
                if let Some(content) = &choice.delta.content {
                    let was_empty = assistant_text.is_empty();
                    assistant_text.push_str(content);
                    if was_empty && looks_like_start_of_tool_call(&assistant_text) {
                        buffering_inline = true;
                    }
                    if !buffering_inline {
                        on_event(AgentEvent::Text {
                            delta: content.clone(),
                        });
                    }
                }
                if let Some(tcs) = &choice.delta.tool_calls {
                    for tc in tcs {
                        let idx = tc.index.unwrap_or(0);
                        let entry = tool_calls
                            .entry(idx)
                            .or_insert_with(|| (String::new(), String::new(), String::new()));
                        if let Some(id) = &tc.id {
                            if !id.is_empty() {
                                entry.0 = id.clone();
                            }
                        }
                        if let Some(f) = &tc.function {
                            if let Some(n) = &f.name {
                                entry.1.push_str(n);
                            }
                            if let Some(a) = &f.arguments {
                                entry.2.push_str(a);
                            }
                        }
                    }
                }
            }
            if let Some(usage) = &chunk.usage {
                total_in += usage.prompt_tokens;
                total_out += usage.completion_tokens;
            }
        })
        .await?;

        let mut final_calls: Vec<ToolCall> = tool_calls
            .into_values()
            .filter(|(id, name, _)| !id.is_empty() && !name.is_empty())
            .map(|(id, name, args)| ToolCall {
                id,
                kind: "function".into(),
                function: ToolFunction {
                    name,
                    arguments: if args.is_empty() { "{}".into() } else { args },
                },
            })
            .collect();

        if final_calls.is_empty() && buffering_inline {
            if let Some((name, args)) = try_parse_inline_tool_call(&assistant_text, &tool_names) {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                final_calls.push(ToolCall {
                    id: format!("inline-{step}-{ts}"),
                    kind: "function".into(),
                    function: ToolFunction {
                        name,
                        arguments: serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                    },
                });
                assistant_text.clear();
            } else {
                on_event(AgentEvent::Text {
                    delta: assistant_text.clone(),
                });
            }
        }

        messages.push(ChatMessage::Assistant {
            content: if assistant_text.is_empty() {
                None
            } else {
                Some(assistant_text.clone())
            },
            tool_calls: final_calls.clone(),
        });

        if final_calls.is_empty() {
            break;
        }

        for tc in final_calls {
            let parsed_args: Value = serde_json::from_str(&tc.function.arguments)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
            on_event(AgentEvent::ToolCall {
                name: tc.function.name.clone(),
                call_id: tc.id.clone(),
                args: parsed_args.clone(),
            });

            let output_str: String = if let Some(tool) = tool_map.get(&tc.function.name) {
                let needs_approval = tool.require_approval(&parsed_args);
                let mut denied = false;
                if needs_approval {
                    let preview = tool.preview(&parsed_args).await;
                    if let Some(provider) = approve.as_deref_mut() {
                        if !provider
                            .approve(&tc.function.name, &parsed_args, preview.as_ref())
                            .await
                        {
                            denied = true;
                        }
                    } else {
                        denied = true;
                    }
                }
                if denied {
                    let denied_str = serde_json::to_string(
                        &serde_json::json!({"error": "User denied execution."}),
                    )
                    .unwrap();
                    on_event(AgentEvent::ToolResult {
                        name: tc.function.name.clone(),
                        call_id: tc.id.clone(),
                        output: denied_str.clone(),
                    });
                    messages.push(ChatMessage::Tool {
                        tool_call_id: tc.id.clone(),
                        content: denied_str,
                    });
                    continue;
                }
                let result = tool.execute_cancelable(parsed_args, cancel.clone()).await;
                if let Some(s) = result.as_str() {
                    s.to_string()
                } else {
                    serde_json::to_string(&result).unwrap_or_else(|_| "null".into())
                }
            } else {
                serde_json::to_string(&serde_json::json!({
                    "error": format!("Unknown tool: {}", tc.function.name)
                }))
                .unwrap()
            };

            let trimmed = compact_tool_output(&tc.function.name, &output_str);
            on_event(AgentEvent::ToolResult {
                name: tc.function.name.clone(),
                call_id: tc.id.clone(),
                output: trimmed.clone(),
            });
            messages.push(ChatMessage::Tool {
                tool_call_id: tc.id,
                content: trimmed,
            });
        }
    }

    Ok(RunResult {
        messages,
        input_tokens: total_in,
        output_tokens: total_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> HashSet<String> {
        ["shell", "file_read", "grep"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn looks_like_tool_call_positives() {
        assert!(looks_like_start_of_tool_call("{\"name\":"));
        assert!(looks_like_start_of_tool_call("  {\"name\": \"shell\""));
        assert!(looks_like_start_of_tool_call(
            "```json\n{\"name\":\"shell\""
        ));
        assert!(looks_like_start_of_tool_call("```\n{\"name\":"));
        assert!(looks_like_start_of_tool_call("{name: \"shell\""));
    }

    #[test]
    fn looks_like_tool_call_negatives() {
        assert!(!looks_like_start_of_tool_call("Hello, how can I help?"));
        assert!(!looks_like_start_of_tool_call(""));
        assert!(!looks_like_start_of_tool_call("Here's the answer:"));
        assert!(!looks_like_start_of_tool_call("{not_name: 1}"));
    }

    #[test]
    fn parse_inline_arguments_field() {
        let n = names();
        let (name, args) =
            try_parse_inline_tool_call(r#"{"name":"shell","arguments":{"command":"ls"}}"#, &n)
                .unwrap();
        assert_eq!(name, "shell");
        assert_eq!(args.get("command").unwrap().as_str().unwrap(), "ls");
    }

    #[test]
    fn parse_inline_parameters_alias() {
        let n = names();
        let (_, args) =
            try_parse_inline_tool_call(r#"{"name":"shell","parameters":{"command":"pwd"}}"#, &n)
                .unwrap();
        assert_eq!(args.get("command").unwrap().as_str().unwrap(), "pwd");
    }

    #[test]
    fn parse_inline_args_alias() {
        let n = names();
        let (_, args) =
            try_parse_inline_tool_call(r#"{"name":"grep","args":{"pattern":"foo"}}"#, &n).unwrap();
        assert_eq!(args.get("pattern").unwrap().as_str().unwrap(), "foo");
    }

    #[test]
    fn parse_inline_with_json_fence() {
        let n = names();
        let r = try_parse_inline_tool_call(
            "```json\n{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n```",
            &n,
        );
        assert!(r.is_some());
    }

    #[test]
    fn parse_inline_with_bare_fence() {
        let n = names();
        let r = try_parse_inline_tool_call(
            "```\n{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n```",
            &n,
        );
        assert!(r.is_some());
    }

    #[test]
    fn parse_inline_unknown_tool_returns_none() {
        let n = names();
        assert!(
            try_parse_inline_tool_call(r#"{"name":"unknown_tool","arguments":{}}"#, &n).is_none()
        );
    }

    #[test]
    fn parse_inline_invalid_json_returns_none() {
        let n = names();
        assert!(try_parse_inline_tool_call("{name", &n).is_none());
        assert!(try_parse_inline_tool_call("not json at all", &n).is_none());
        assert!(try_parse_inline_tool_call("", &n).is_none());
    }

    #[test]
    fn parse_inline_missing_name_returns_none() {
        let n = names();
        assert!(try_parse_inline_tool_call(r#"{"arguments":{}}"#, &n).is_none());
    }

    #[test]
    fn parse_inline_no_args_defaults_to_empty_object() {
        let n = names();
        let (_, args) = try_parse_inline_tool_call(r#"{"name":"shell"}"#, &n).unwrap();
        assert!(args.is_object());
    }

    #[test]
    fn compacts_large_json_content_field() {
        let output = serde_json::json!({
            "content": "x".repeat(5000),
            "totalLines": 1
        })
        .to_string();
        let compacted = compact_tool_output("file_read", &output);
        let parsed: Value = serde_json::from_str(&compacted).unwrap();
        assert_eq!(parsed["compacted"].as_bool(), Some(true));
        assert!(parsed["content"].as_str().unwrap().contains("[truncated]"));
    }

    #[test]
    fn compacts_large_match_arrays() {
        let matches: Vec<Value> = (0..1000)
            .map(|i| serde_json::json!({ "line": i }))
            .collect();
        let output = serde_json::json!({ "matches": matches, "count": 1000 }).to_string();
        let compacted = compact_tool_output("grep", &output);
        let parsed: Value = serde_json::from_str(&compacted).unwrap();
        assert_eq!(parsed["compacted"].as_bool(), Some(true));
        assert_eq!(parsed["matches"].as_array().unwrap().len(), 50);
    }
}

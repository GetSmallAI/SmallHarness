use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

static API_KEY_RE: OnceLock<Regex> = OnceLock::new();

fn api_key_pattern() -> &'static Regex {
    API_KEY_RE.get_or_init(|| {
        Regex::new(r"(?i)(sk-[a-z0-9_-]{8,}|sk-or-[a-z0-9_-]{8,})").expect("api key regex")
    })
}

/// Sidecar path for a chat session transcript: `<stem>.events.jsonl`.
pub fn events_path_for_session(session_path: &Path) -> PathBuf {
    session_path.with_extension("events.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLogConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for EventLogConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Allowed,
    Denied,
    Always,
    Session,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnMetrics {
    pub steps: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u128>,
    pub model_ms: u128,
    pub tool_ms: u128,
    pub approval_ms: u128,
    pub total_ms: u128,
    pub hit_step_limit: bool,
}

impl TurnMetrics {
    pub fn format_footer_suffix(&self) -> String {
        if self.steps == 0 && self.total_ms == 0 {
            return String::new();
        }
        let mut parts = vec![format!("{} steps", self.steps)];
        if let Some(ttft) = self.ttft_ms {
            parts.push(format!("TTFT {:.1}s", ttft as f64 / 1000.0));
        }
        if self.model_ms > 0 {
            parts.push(format!("model {:.1}s", self.model_ms as f64 / 1000.0));
        }
        if self.tool_ms > 0 {
            parts.push(format!("tools {:.1}s", self.tool_ms as f64 / 1000.0));
        }
        if self.approval_ms > 0 {
            parts.push(format!("approval {:.1}s", self.approval_ms as f64 / 1000.0));
        }
        if self.total_ms > 0 {
            parts.push(format!("total {:.1}s", self.total_ms as f64 / 1000.0));
        }
        format!(" · {}", parts.join(" · "))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TracePayload {
    ToolCall {
        call_id: String,
        name: String,
        args: Value,
        depth: u32,
    },
    ToolResult {
        call_id: String,
        name: String,
        duration_ms: u128,
        #[serde(default)]
        compacted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        compact_summary: Option<String>,
        depth: u32,
    },
    Approval {
        tool: String,
        decision: ApprovalDecision,
        cache_key: String,
        duration_ms: u128,
    },
    ContextCompacted {
        method: String,
        before_msgs: usize,
        after_msgs: usize,
    },
    SubagentStart {
        call_id: String,
        task: String,
    },
    SubagentEnd {
        call_id: String,
        input_tokens: u32,
        output_tokens: u32,
        duration_ms: u128,
    },
    Warmup {
        duration_ms: u128,
        reason: String,
    },
    TurnSummary(TurnMetrics),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceLine {
    timestamp: String,
    turn: u32,
    #[serde(flatten)]
    payload: TracePayload,
}

pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_string(s)),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key_lower = k.to_ascii_lowercase();
                if key_lower.contains("api_key")
                    || key_lower.contains("apikey")
                    || key_lower.contains("token")
                    || key_lower.contains("secret")
                    || key_lower.contains("password")
                {
                    out.insert(k.clone(), Value::String("(redacted)".into()));
                } else {
                    out.insert(k.clone(), redact_value(v));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

pub fn redact_string(s: &str) -> String {
    api_key_pattern().replace_all(s, "(redacted)").into_owned()
}

pub struct TurnTrace {
    path: PathBuf,
    turn: u32,
    turn_started: Instant,
    enabled: bool,
}

impl TurnTrace {
    pub fn open(session_path: &Path, enabled: bool) -> Result<Self> {
        let path = events_path_for_session(session_path);
        if enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Ok(Self {
            path,
            turn: 0,
            turn_started: Instant::now(),
            enabled,
        })
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[allow(dead_code)]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn begin_turn(&mut self) {
        self.turn = self.turn.saturating_add(1);
        self.turn_started = Instant::now();
    }

    #[allow(dead_code)]
    pub fn current_turn(&self) -> u32 {
        self.turn
    }

    pub fn append(&self, payload: TracePayload) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let line = TraceLine {
            timestamp: Utc::now().to_rfc3339(),
            turn: self.turn,
            payload,
        };
        let json = serde_json::to_string(&line)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(json.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }

    pub fn log_turn_summary(&self, mut metrics: TurnMetrics) -> Result<()> {
        metrics.total_ms = self.turn_started.elapsed().as_millis();
        self.append(TracePayload::TurnSummary(metrics))
    }
}

pub type SharedTurnTrace = Arc<Mutex<TurnTrace>>;

pub fn shared_trace(session_path: &Path, enabled: bool) -> Result<SharedTurnTrace> {
    Ok(Arc::new(Mutex::new(TurnTrace::open(
        session_path,
        enabled,
    )?)))
}

pub fn sync_trace_path(session_path: &Path, trace: &SharedTurnTrace) -> Result<()> {
    let mut guard = trace.lock().expect("turn trace mutex");
    guard.path = events_path_for_session(session_path);
    Ok(())
}

/// Test helper: disabled event log on a throwaway session path.
#[cfg(test)]
pub fn test_trace_for(session_path: &Path) -> SharedTurnTrace {
    shared_trace(session_path, false).expect("test trace")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn redacts_api_keys_in_strings() {
        let s = redact_string("use sk-1234567890abcdef for auth");
        assert!(!s.contains("sk-1234567890abcdef"));
        assert!(s.contains("(redacted)"));
    }

    #[test]
    fn redacts_sensitive_object_keys() {
        let v = serde_json::json!({ "api_key": "sk-secret", "path": "src/main.rs" });
        let out = redact_value(&v);
        assert_eq!(out["api_key"], "(redacted)");
        assert_eq!(out["path"], "src/main.rs");
    }

    #[test]
    fn append_writes_jsonl_lines() {
        let dir = TempDir::new().unwrap();
        let session = dir.path().join("2024-01-01.jsonl");
        let mut trace = TurnTrace::open(&session, true).unwrap();
        trace.begin_turn();
        trace
            .append(TracePayload::Warmup {
                duration_ms: 42,
                reason: "test".into(),
            })
            .unwrap();
        let text = std::fs::read_to_string(events_path_for_session(&session)).unwrap();
        assert!(text.contains("\"kind\":\"warmup\""));
        assert!(text.contains("\"turn\":1"));
    }

    #[test]
    fn footer_suffix_formats_timing() {
        let m = TurnMetrics {
            steps: 4,
            ttft_ms: Some(900),
            model_ms: 3200,
            tool_ms: 5100,
            approval_ms: 0,
            total_ms: 9400,
            hit_step_limit: false,
        };
        let s = m.format_footer_suffix();
        assert!(s.contains("4 steps"));
        assert!(s.contains("TTFT"));
        assert!(s.contains("model"));
        assert!(s.contains("tools"));
    }
}

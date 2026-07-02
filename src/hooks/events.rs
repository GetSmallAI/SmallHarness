use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookEventName {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PreCompact,
    PostCompact,
    PlanUpdated,
    SubagentStart,
    SubagentStop,
    Stop,
    SessionEnd,
}

impl HookEventName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PermissionRequest => "PermissionRequest",
            Self::PostToolUse => "PostToolUse",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::PlanUpdated => "PlanUpdated",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
        }
    }

    pub fn key_label(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::PreToolUse => "pre_tool_use",
            Self::PermissionRequest => "permission_request",
            Self::PostToolUse => "post_tool_use",
            Self::PreCompact => "pre_compact",
            Self::PostCompact => "post_compact",
            Self::PlanUpdated => "plan_updated",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
            Self::Stop => "stop",
            Self::SessionEnd => "session_end",
        }
    }

    pub fn supports_matcher(self) -> bool {
        matches!(
            self,
            Self::SessionStart
                | Self::PreToolUse
                | Self::PermissionRequest
                | Self::PostToolUse
                | Self::PreCompact
                | Self::PostCompact
                | Self::SubagentStart
                | Self::SubagentStop
        )
    }
}

#[derive(Debug, Clone)]
pub struct HookPayload {
    value: serde_json::Map<String, Value>,
}

impl HookPayload {
    pub fn new(event: HookEventName, session_id: impl Into<String>) -> Self {
        let mut value = serde_json::Map::new();
        value.insert("hook_event_name".into(), json!(event.as_str()));
        value.insert("session_id".into(), json!(session_id.into()));
        Self { value }
    }

    pub fn turn_id(mut self, turn_id: u32) -> Self {
        self.value.insert("turn_id".into(), json!(turn_id));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.value.insert("cwd".into(), json!(cwd.into()));
        self
    }

    pub fn transcript_path(mut self, path: impl Into<String>) -> Self {
        self.value
            .insert("transcript_path".into(), json!(path.into()));
        self
    }

    pub fn insert(mut self, key: impl Into<String>, value: Value) -> Self {
        self.value.insert(key.into(), value);
        self
    }

    pub fn into_value(self) -> Value {
        Value::Object(self.value)
    }
}

#[derive(Debug, Clone)]
pub struct HookInvocationContext {
    pub session_id: String,
    pub turn_id: u32,
    pub cwd: String,
    pub workspace_root: String,
    pub transcript_path: String,
    pub events_path: String,
    pub backend: String,
    pub model: String,
    pub approval_policy: String,
    pub source: String,
}

impl HookInvocationContext {
    pub fn payload(&self, event: HookEventName) -> HookPayload {
        HookPayload::new(event, self.session_id.clone())
            .turn_id(self.turn_id)
            .cwd(self.cwd.clone())
            .transcript_path(self.transcript_path.clone())
            .insert("workspace_root", json!(self.workspace_root))
            .insert("events_path", json!(self.events_path))
            .insert("backend", json!(self.backend))
            .insert("model", json!(self.model))
            .insert("approval_policy", json!(self.approval_policy))
            .insert("source", json!(self.source))
    }
}

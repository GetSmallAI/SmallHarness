use regex::Regex;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::{HookCommandConfig, HookConfig, HookEventName, HookState};

#[derive(Debug, Clone, Default)]
pub struct HookStateStore {
    pub user: BTreeMap<String, HookState>,
}

impl HookStateStore {
    fn state_for(&self, key: &str) -> Option<&HookState> {
        self.user.get(key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum HookSourceKind {
    User,
    Project,
    ManagedLaunch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSource {
    pub kind: HookSourceKind,
    pub label: String,
}

impl HookSource {
    pub fn project(label: impl Into<String>) -> Self {
        Self {
            kind: HookSourceKind::Project,
            label: label.into(),
        }
    }

    pub fn managed_launch(label: impl Into<String>) -> Self {
        Self {
            kind: HookSourceKind::ManagedLaunch,
            label: label.into(),
        }
    }

    fn is_managed(&self) -> bool {
        self.kind == HookSourceKind::ManagedLaunch
    }

    fn key_prefix(&self) -> &'static str {
        match self.kind {
            HookSourceKind::User => "user",
            HookSourceKind::Project => "project",
            HookSourceKind::ManagedLaunch => "managed-launch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTrustStatus {
    Managed,
    Trusted,
    Modified,
    Untrusted,
    Disabled,
    Invalid,
}

#[derive(Debug, Clone)]
pub struct DiscoveredHook {
    pub key: String,
    pub event: HookEventName,
    pub matcher: Option<String>,
    pub handler: HookCommandConfig,
    pub source: HookSource,
    pub current_hash: String,
    pub trust_status: HookTrustStatus,
    pub display_order: usize,
    pub matcher_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HookDiscovery {
    pub entries: Vec<DiscoveredHook>,
    pub runnable: Vec<DiscoveredHook>,
}

pub fn discover_hooks(
    config: &HookConfig,
    source: HookSource,
    states: &HookStateStore,
) -> HookDiscovery {
    let mut out = HookDiscovery::default();
    for event in all_hook_events() {
        for (group_idx, group) in config.groups_for(event).iter().enumerate() {
            let matcher = matcher_pattern_for_event(event, group.matcher.as_deref());
            let matcher_error = matcher
                .and_then(|matcher| validate_matcher_pattern(matcher).err())
                .map(|err| format!("invalid matcher regex: {err}"));
            for (hook_idx, handler) in group.hooks.iter().enumerate() {
                if handler.async_handler || handler.command.trim().is_empty() {
                    continue;
                }
                let handler = normalize_handler(handler);
                let key = format!(
                    "{}:{}:{}:{}:{}",
                    source.key_prefix(),
                    source.label,
                    event.key_label(),
                    group_idx,
                    hook_idx
                );
                let current_hash = hook_hash(event, matcher, &handler);
                let state = states.state_for(&key);
                let enabled = state.and_then(|s| s.enabled).unwrap_or(true);
                let trust_status = if matcher_error.is_some() {
                    HookTrustStatus::Invalid
                } else if !enabled {
                    HookTrustStatus::Disabled
                } else if source.is_managed() {
                    HookTrustStatus::Managed
                } else {
                    match state.and_then(|s| s.trusted_hash.as_deref()) {
                        Some(hash) if hash == current_hash => HookTrustStatus::Trusted,
                        Some(_) => HookTrustStatus::Modified,
                        None => HookTrustStatus::Untrusted,
                    }
                };
                let entry = DiscoveredHook {
                    key,
                    event,
                    matcher: matcher.map(str::to_string),
                    handler,
                    source: source.clone(),
                    current_hash,
                    trust_status,
                    display_order: out.entries.len(),
                    matcher_error: matcher_error.clone(),
                };
                if matches!(
                    entry.trust_status,
                    HookTrustStatus::Managed | HookTrustStatus::Trusted
                ) {
                    out.runnable.push(entry.clone());
                }
                out.entries.push(entry);
            }
        }
    }
    out
}

fn all_hook_events() -> [HookEventName; 12] {
    [
        HookEventName::SessionStart,
        HookEventName::UserPromptSubmit,
        HookEventName::PreToolUse,
        HookEventName::PermissionRequest,
        HookEventName::PostToolUse,
        HookEventName::PreCompact,
        HookEventName::PostCompact,
        HookEventName::PlanUpdated,
        HookEventName::SubagentStart,
        HookEventName::SubagentStop,
        HookEventName::Stop,
        HookEventName::SessionEnd,
    ]
}

fn hook_hash(event: HookEventName, matcher: Option<&str>, handler: &HookCommandConfig) -> String {
    let identity = json!({
        "event": event.key_label(),
        "matcher": matcher,
        "type": "command",
        "command": handler.command,
        "commandWindows": serde_json::Value::Null,
        "timeoutSec": handler.timeout_sec,
        "async": handler.async_handler,
        "statusMessage": handler.status_message,
        "env": handler.env,
        "envVars": handler.env_vars,
    });
    let bytes = serde_json::to_vec(&identity).expect("hook identity serializes");
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", hex_lower(&digest))
}

fn normalize_handler(handler: &HookCommandConfig) -> HookCommandConfig {
    let command = if cfg!(windows) {
        handler
            .command_windows
            .clone()
            .unwrap_or_else(|| handler.command.clone())
    } else {
        handler.command.clone()
    };
    HookCommandConfig {
        command,
        timeout_sec: handler.timeout_sec.max(1),
        command_windows: None,
        status_message: handler.status_message.clone(),
        async_handler: false,
        env: handler.env.clone(),
        env_vars: handler.env_vars.clone(),
    }
}

fn matcher_pattern_for_event(event: HookEventName, matcher: Option<&str>) -> Option<&str> {
    event
        .supports_matcher()
        .then_some(matcher)
        .flatten()
        .map(str::trim)
        .filter(|matcher| !matcher.is_empty())
}

fn validate_matcher_pattern(matcher: &str) -> Result<(), regex::Error> {
    if is_match_all_matcher(matcher) || is_exact_matcher(matcher) {
        return Ok(());
    }
    full_match_regex(matcher).map(|_| ())
}

pub fn matcher_matches(matcher: Option<&str>, value: Option<&str>) -> bool {
    let Some(matcher) = matcher.map(str::trim).filter(|m| !m.is_empty()) else {
        return true;
    };
    if is_match_all_matcher(matcher) {
        return true;
    }
    let Some(value) = value else {
        return false;
    };
    if is_exact_matcher(matcher) {
        return matcher.split('|').any(|part| part.trim() == value);
    }
    full_match_regex(matcher)
        .map(|regex| regex.is_match(value))
        .unwrap_or(false)
}

fn full_match_regex(matcher: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{matcher})$"))
}

fn is_match_all_matcher(matcher: &str) -> bool {
    matcher == "*"
}

fn is_exact_matcher(matcher: &str) -> bool {
    matcher
        .split('|')
        .all(|part| !part.is_empty() && part.chars().all(is_exact_matcher_char))
}

fn is_exact_matcher_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

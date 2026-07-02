use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Block,
    Allow,
    Deny,
    Stop,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookEffect {
    pub decision: Option<HookDecision>,
    pub reason: Option<String>,
    pub additional_context: Option<String>,
    pub updated_input: Option<Value>,
    pub feedback: Option<String>,
    pub warning: Option<String>,
}

pub fn parse_hook_effect(exit_code: i32, stdout: &str, stderr: &str) -> HookEffect {
    let mut effect = HookEffect::default();
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        match serde_json::from_str::<Value>(stdout) {
            Ok(value) => apply_effect_json(&mut effect, &value),
            Err(e) => {
                effect.warning = Some(format!("invalid hook JSON: {e}"));
            }
        }
    }
    if exit_code == 2 && effect.decision.is_none() {
        effect.decision = Some(HookDecision::Block);
        let reason = stderr.trim();
        if !reason.is_empty() {
            effect.reason = Some(reason.to_string());
        }
    } else if exit_code != 0 && effect.warning.is_none() {
        effect.warning = Some(format!("hook exited with status {exit_code}"));
    }
    effect
}

fn apply_effect_json(effect: &mut HookEffect, value: &Value) {
    let Some(obj) = value.as_object() else {
        return;
    };
    effect.decision = obj
        .get("decision")
        .and_then(Value::as_str)
        .and_then(parse_decision);
    effect.reason = obj
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    effect.additional_context = obj
        .get("additionalContext")
        .and_then(Value::as_str)
        .map(str::to_string);
    effect.updated_input = obj.get("updatedInput").cloned();
    effect.feedback = obj
        .get("feedback")
        .and_then(Value::as_str)
        .map(str::to_string);
}

fn parse_decision(raw: &str) -> Option<HookDecision> {
    match raw {
        "block" => Some(HookDecision::Block),
        "allow" => Some(HookDecision::Allow),
        "deny" => Some(HookDecision::Deny),
        "stop" => Some(HookDecision::Stop),
        _ => None,
    }
}

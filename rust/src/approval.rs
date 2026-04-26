use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;

use crate::agent::ApprovalProvider;
use crate::input::plain_read_line;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";

pub struct ApprovalCache {
    pub always_allow: HashSet<String>,
}

impl ApprovalCache {
    pub fn new() -> Self {
        Self {
            always_allow: HashSet::new(),
        }
    }
}

fn summarize(name: &str, args: &Value) -> String {
    match name {
        "shell" => args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "file_write" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or("");
            let bytes = args
                .get("content")
                .and_then(Value::as_str)
                .map(|s| s.as_bytes().len())
                .unwrap_or(0);
            format!("{path} ({bytes} bytes)")
        }
        "file_edit" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or("");
            let edits = args
                .get("edits")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            let plural = if edits == 1 { "" } else { "s" };
            format!("{path} ({edits} edit{plural})")
        }
        _ => args.to_string(),
    }
}

#[async_trait]
impl ApprovalProvider for ApprovalCache {
    async fn approve(&mut self, name: &str, args: &Value) -> bool {
        let cache_key = format!(
            "{name}:{}",
            args.get("command")
                .and_then(Value::as_str)
                .or_else(|| args.get("path").and_then(Value::as_str))
                .unwrap_or("")
        );
        if self.always_allow.contains(name) || self.always_allow.contains(&cache_key) {
            return true;
        }
        let summary = summarize(name, args);
        println!();
        println!("  {YELLOW}▲{RESET} {BOLD}Approval required{RESET} {DIM}for{RESET} {BOLD}{name}{RESET}");
        println!("    {DIM}{summary}{RESET}");
        println!(
            "    {DIM}[y]es · [n]o · [a]lways for {name} · [s]ession-allow this exact call{RESET}"
        );
        let prompt = format!("  {YELLOW}? {RESET}");
        let answer = match plain_read_line(prompt).await {
            Ok(a) => a.trim().to_lowercase(),
            Err(_) => return false,
        };

        if answer == "a" || answer == "always" {
            self.always_allow.insert(name.to_string());
            println!("  {GREEN}✓{RESET} {DIM}allowing all {name} calls this session{RESET}");
            return true;
        }
        if answer == "s" {
            self.always_allow.insert(cache_key);
            return true;
        }
        if answer == "y" || answer == "yes" {
            return true;
        }
        println!("  {RED}✗{RESET} {DIM}denied{RESET}");
        false
    }
}

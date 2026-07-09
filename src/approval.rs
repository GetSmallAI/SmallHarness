use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;

use crate::agent::ApprovalProvider;
use crate::input::plain_read_line;
use crate::tools::ToolPreview;

use crate::theme::{FAIL, OK, WARN_MARK};

const RESET: crate::theme::Style = crate::theme::RESET;
const BOLD: crate::theme::Style = crate::theme::BOLD;
const DIM: crate::theme::Style = crate::theme::MUTED;
const YELLOW: crate::theme::Style = crate::theme::WARN;
const RED: crate::theme::Style = crate::theme::ERROR;
const GREEN: crate::theme::Style = crate::theme::SUCCESS;

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
                .map(|s| s.len())
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
    async fn approve(&mut self, name: &str, args: &Value, preview: Option<&ToolPreview>) -> bool {
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
        let summary = preview
            .map(|p| p.summary.clone())
            .unwrap_or_else(|| summarize(name, args));
        println!();
        println!(
            "  {YELLOW}{WARN_MARK}{RESET} {BOLD}Approval required{RESET} {DIM}for{RESET} {BOLD}{name}{RESET}"
        );
        println!("    {DIM}{summary}{RESET}");
        if let Some(preview) = preview {
            if let Some(risk) = &preview.risk {
                println!("    {RED}risk:{RESET} {DIM}{risk}{RESET}");
            }
            if let Some(diff) = &preview.diff {
                println!();
                crate::diff_view::print_diff(diff, 80);
                println!();
            }
        }
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
            println!("  {GREEN}{OK}{RESET} {DIM}allowing all {name} calls this session{RESET}");
            return true;
        }
        if answer == "s" {
            self.always_allow.insert(cache_key);
            return true;
        }
        if answer == "y" || answer == "yes" {
            return true;
        }
        println!("  {RED}{FAIL}{RESET} {DIM}denied{RESET}");
        false
    }
}

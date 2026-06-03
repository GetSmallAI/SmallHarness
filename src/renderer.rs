use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use crate::agent::AgentEvent;
use crate::config::{DisplayConfig, ToolDisplay};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const GRAY: &str = "\x1b[90m";
const MAGENTA: &str = "\x1b[35m";

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

fn formatter_for(name: &str, args: &Value) -> String {
    match name {
        "shell" => {
            let s = args.get("command").and_then(Value::as_str).unwrap_or("");
            format!("command={}", trunc(s, 50))
        }
        "file_read" | "file_write" | "file_edit" => {
            let s = args.get("path").and_then(Value::as_str).unwrap_or("");
            format!("path={}", trunc(s, 50))
        }
        "glob" | "grep" => {
            let s = args.get("pattern").and_then(Value::as_str).unwrap_or("");
            format!("pattern={}", trunc(s, 50))
        }
        "list_dir" => {
            let s = args.get("path").and_then(Value::as_str).unwrap_or(".");
            format!("path={}", trunc(s, 50))
        }
        "task" => {
            let s = args.get("task").and_then(Value::as_str).unwrap_or("");
            trunc(s, 60)
        }
        _ => default_format(args),
    }
}

fn default_format(args: &Value) -> String {
    if let Some(obj) = args.as_object() {
        if let Some((k, v)) = obj.iter().next() {
            let vs = match v {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            return format!("{k}={}", trunc(&vs, 50));
        }
    }
    String::new()
}

fn label_past(name: &str) -> &'static str {
    match name {
        "shell" => "Ran",
        "file_read" => "Read",
        "file_write" => "Wrote",
        "file_edit" => "Edited",
        "glob" => "Explored",
        "grep" => "Searched",
        "list_dir" => "Listed",
        "task" => "Delegated",
        _ => "Used",
    }
}

fn label_noun(name: &str) -> &'static str {
    match name {
        "shell" => "shell command",
        "file_read" | "file_write" | "file_edit" => "file",
        "glob" | "grep" => "pattern",
        "list_dir" => "directory",
        _ => "tool",
    }
}

fn tool_color(name: &str) -> &'static str {
    match name {
        "shell" => RED,
        "file_write" | "file_edit" => YELLOW,
        "grep" => MAGENTA,
        _ => YELLOW,
    }
}

fn summarize_output(output: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<Value>(output) {
        if let Some(err) = parsed.get("error").and_then(Value::as_str) {
            return format!("{RED}error: {}{RESET}", trunc(err, 60));
        }
        if let Some(n) = parsed.get("totalLines").and_then(Value::as_u64) {
            return format!("{n} lines");
        }
        if let Some(n) = parsed.get("count").and_then(Value::as_u64) {
            let kind = if parsed.get("matches").is_some() {
                "matches"
            } else {
                "entries"
            };
            return format!("{n} {kind}");
        }
        if let Some(summary) = parsed.get("summary").and_then(Value::as_str) {
            let first_line = summary.split('\n').next().unwrap_or("");
            return trunc(first_line, 60);
        }
        if parsed.get("written").is_some() {
            let bytes = parsed.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            return format!("wrote {bytes} bytes");
        }
        if parsed.get("edited").is_some() {
            return "edited".to_string();
        }
        if let Some(code) = parsed.get("exitCode").and_then(Value::as_i64) {
            let to = if parsed
                .get("timedOut")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                " (timeout)"
            } else {
                ""
            };
            return format!("exit {code}{to}");
        }
    }
    let first_line = output.split('\n').next().unwrap_or("");
    trunc(first_line, 60)
}

struct PendingCall {
    name: String,
    call_id: String,
    args: Value,
    output: Option<String>,
}

pub struct TuiRenderer {
    display: DisplayConfig,
    tool_start: HashMap<String, Instant>,
    streaming: bool,
    grouped_pending: Vec<PendingCall>,
    grouped_category: String,
    minimal_batch: BTreeMap<String, usize>,
    /// Per-turn flag: have we already printed the "thinking…" header for the
    /// current burst of reasoning deltas? Reset at end_turn so each turn
    /// gets its own header.
    reasoning_header_shown: bool,
}

impl TuiRenderer {
    pub fn new(display: DisplayConfig) -> Self {
        Self {
            display,
            tool_start: HashMap::new(),
            streaming: false,
            grouped_pending: Vec::new(),
            grouped_category: String::new(),
            minimal_batch: BTreeMap::new(),
            reasoning_header_shown: false,
        }
    }

    /// Toggle reasoning panel visibility at runtime. Returns the new state.
    pub fn set_reasoning(&mut self, on: bool) -> bool {
        self.display.reasoning = on;
        on
    }

    pub fn reasoning_enabled(&self) -> bool {
        self.display.reasoning
    }

    pub fn handle(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Text { delta } => self.render_text(&delta),
            AgentEvent::ToolCall {
                name,
                call_id,
                args,
            } => self.render_tool_call(&name, &call_id, args),
            AgentEvent::ToolResult {
                name,
                call_id,
                output,
            } => self.render_tool_result(&name, &call_id, &output),
            AgentEvent::Reasoning { delta } => self.render_reasoning(&delta),
            AgentEvent::ContextCompacted { notice, .. } => {
                println!("{notice}");
            }
            AgentEvent::StepLimitReached { max_steps } => {
                self.end_streaming();
                println!(
                    "  {YELLOW}⚠ stopped after {max_steps} steps (step budget){RESET} {DIM}— the task may be unfinished. Send \"continue\" to resume, or raise maxSteps in config.{RESET}"
                );
            }
        }
    }

    pub fn end_turn(&mut self) {
        self.flush_grouped();
        self.flush_minimal();
        self.end_reasoning();
        self.end_streaming();
        self.reasoning_header_shown = false;
    }

    /// Close out the reasoning panel before switching to other output (text,
    /// tool calls). Adds a trailing newline only if we actually printed one.
    fn end_reasoning(&mut self) {
        if self.reasoning_header_shown {
            let mut out = std::io::stdout();
            let _ = writeln!(out, "{RESET}");
            let _ = out.flush();
        }
    }

    fn end_streaming(&mut self) {
        if self.streaming {
            let mut out = std::io::stdout();
            let _ = writeln!(out, "{RESET}");
            let _ = out.flush();
            self.streaming = false;
        }
    }

    fn render_text(&mut self, delta: &str) {
        self.flush_minimal();
        // Switching from reasoning to answer text — close the panel cleanly
        // so the answer doesn't appear glued to the dim trace.
        if self.reasoning_header_shown {
            self.end_reasoning();
            self.reasoning_header_shown = false;
        }
        self.streaming = true;
        let mut out = std::io::stdout();
        let _ = write!(out, "{delta}");
        let _ = out.flush();
    }

    fn render_reasoning(&mut self, delta: &str) {
        if !self.display.reasoning {
            return;
        }
        self.end_streaming();
        let mut out = std::io::stdout();
        if !self.reasoning_header_shown {
            let _ = writeln!(out, "{GRAY}  thinking…{RESET}");
            let _ = write!(out, "{DIM}  ");
            self.reasoning_header_shown = true;
        }
        let dimmed = delta.replace('\n', "\n  ");
        let _ = write!(out, "{dimmed}");
        let _ = out.flush();
    }

    fn render_tool_call(&mut self, name: &str, call_id: &str, args: Value) {
        if matches!(self.display.tool_display, ToolDisplay::Hidden) {
            return;
        }
        // The plan is rendered as a checklist at call time (the args carry the
        // plan), independent of the configured tool display style, and never
        // routed through the grouped/minimal batching.
        if name == "update_plan" {
            self.render_plan(&args);
            return;
        }
        self.end_streaming();
        self.tool_start.insert(call_id.to_string(), Instant::now());

        match self.display.tool_display {
            ToolDisplay::Emoji => {
                let color = tool_color(name);
                let arg_str = formatter_for(name, &args);
                let sep = if arg_str.is_empty() { "" } else { " " };
                println!("  {color}⚡{RESET} {DIM}{name}{sep}{arg_str}{RESET}");
            }
            ToolDisplay::Grouped => {
                let category = label_past(name).to_string();
                if category != self.grouped_category {
                    self.flush_grouped();
                    self.grouped_category = category;
                }
                self.grouped_pending.push(PendingCall {
                    name: name.to_string(),
                    call_id: call_id.to_string(),
                    args,
                    output: None,
                });
            }
            ToolDisplay::Minimal => {
                *self.minimal_batch.entry(name.to_string()).or_insert(0) += 1;
            }
            ToolDisplay::Hidden => {}
        }
    }

    /// Draw the model's task plan as a checklist box. Flushes any in-flight
    /// grouped/minimal output first so the plan sits on its own.
    fn render_plan(&mut self, args: &Value) {
        self.flush_grouped();
        self.flush_minimal();
        self.end_streaming();
        let Some(steps) = args.get("steps").and_then(Value::as_array) else {
            return;
        };
        if steps.is_empty() {
            return;
        }
        let done = steps
            .iter()
            .filter(|s| s.get("status").and_then(Value::as_str) == Some("done"))
            .count();
        println!("{GREEN}●{RESET} {BOLD}Plan{RESET} {DIM}({done}/{} done){RESET}", steps.len());
        for s in steps {
            let text = s.get("step").and_then(Value::as_str).unwrap_or("");
            let status = s.get("status").and_then(Value::as_str).unwrap_or("pending");
            let (mark, body) = match status {
                "done" => (format!("{GREEN}✔{RESET}"), format!("{DIM}{text}{RESET}")),
                "in_progress" => (format!("{YELLOW}▸{RESET}"), format!("{BOLD}{text}{RESET}")),
                _ => (format!("{GRAY}☐{RESET}"), format!("{GRAY}{text}{RESET}")),
            };
            println!("  {mark} {body}");
        }
        println!();
    }

    fn render_tool_result(&mut self, name: &str, call_id: &str, output: &str) {
        if matches!(self.display.tool_display, ToolDisplay::Hidden) {
            return;
        }
        // The plan was already drawn at call time; its result carries no new
        // information for the user.
        if name == "update_plan" {
            return;
        }
        let ms = self
            .tool_start
            .get(call_id)
            .map(|s| s.elapsed().as_millis() as f64 / 1000.0)
            .unwrap_or(0.0);
        let dur = format!("({:.1}s)", ms);

        match self.display.tool_display {
            ToolDisplay::Emoji => {
                println!("  {GREEN}✓{RESET} {DIM}{name} {dur}{RESET}");
            }
            ToolDisplay::Grouped => {
                if let Some(p) = self
                    .grouped_pending
                    .iter_mut()
                    .find(|p| p.call_id == call_id)
                {
                    p.output = Some(output.to_string());
                }
            }
            _ => {}
        }
    }

    fn flush_grouped(&mut self) {
        if self.grouped_pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.grouped_pending);
        let first = &pending[0];
        let label = label_past(&first.name);

        if pending.len() == 1 {
            let arg_str = formatter_for(&first.name, &first.args);
            println!("{GREEN}●{RESET} {BOLD}{label}{RESET} {DIM}{arg_str}{RESET}");
            if let Some(out) = &first.output {
                let summary = summarize_output(out);
                if !summary.is_empty() {
                    println!("  {GRAY}└ {summary}{RESET}");
                }
            }
        } else {
            println!("{GREEN}●{RESET} {BOLD}{label}{RESET}");
            let n = pending.len();
            for (i, p) in pending.iter().enumerate() {
                let is_last = i == n - 1;
                let branch = if is_last { "└" } else { "├" };
                let arg_str = formatter_for(&p.name, &p.args);
                let summary = p
                    .output
                    .as_ref()
                    .map(|o| format!(" {GRAY}{}{RESET}", summarize_output(o)))
                    .unwrap_or_default();
                println!("  {GRAY}{branch}{RESET} {DIM}{arg_str}{RESET}{summary}");
            }
        }
        println!();
        self.grouped_category.clear();
    }

    fn flush_minimal(&mut self) {
        if self.minimal_batch.is_empty() {
            return;
        }
        let parts: Vec<String> = self
            .minimal_batch
            .iter()
            .map(|(name, count)| {
                let past = label_past(name).to_lowercase();
                let noun = label_noun(name);
                let plural = if *count == 1 { "" } else { "s" };
                format!("{past} {count} {noun}{plural}")
            })
            .collect();
        println!("  {GRAY}{}{RESET}", parts.join(", "));
        self.minimal_batch.clear();
    }
}

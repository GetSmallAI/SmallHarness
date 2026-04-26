use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const GRAY: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEntry {
    value: String,
}

#[derive(Debug, Clone)]
pub struct InputHistory {
    path: String,
    max_entries: usize,
    entries: Vec<String>,
    enabled: bool,
}

impl InputHistory {
    pub fn load(path: String, max_entries: usize, enabled: bool) -> Self {
        let mut entries = Vec::new();
        if enabled {
            if let Ok(text) = fs::read_to_string(&path) {
                for line in text.lines() {
                    if let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) {
                        if !entry.value.trim().is_empty() {
                            entries.push(entry.value);
                        }
                    }
                }
            }
        }
        let max_entries = max_entries.max(1);
        if entries.len() > max_entries {
            entries = entries[entries.len() - max_entries..].to_vec();
        }
        Self {
            path,
            max_entries,
            entries,
            enabled,
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn push(&mut self, value: &str) -> Result<()> {
        if !self.enabled || value.trim().is_empty() {
            return Ok(());
        }
        if self
            .entries
            .last()
            .map(|last| last == value)
            .unwrap_or(false)
        {
            return Ok(());
        }
        self.entries.push(value.to_string());
        if self.entries.len() > self.max_entries {
            self.entries.remove(0);
        }
        if let Some(parent) = Path::new(&self.path).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(&HistoryEntry {
            value: value.to_string(),
        })?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }
}

pub async fn bordered_read_line(history: Vec<String>) -> Result<String> {
    tokio::task::spawn_blocking(move || read_bordered(&history)).await?
}

pub async fn plain_read_line(prompt: String) -> Result<String> {
    plain_read_line_with_history(prompt, Vec::new()).await
}

pub async fn plain_read_line_with_history(prompt: String, history: Vec<String>) -> Result<String> {
    tokio::task::spawn_blocking(move || read_plain(&prompt, &history)).await?
}

fn render_value(value: &str) -> String {
    value.replace('\n', "⏎")
}

fn prev_word(chars: &[char], mut cursor: usize) -> usize {
    while cursor > 0 && chars[cursor - 1].is_whitespace() {
        cursor -= 1;
    }
    while cursor > 0 && !chars[cursor - 1].is_whitespace() {
        cursor -= 1;
    }
    cursor
}

fn next_word(chars: &[char], mut cursor: usize) -> usize {
    while cursor < chars.len() && !chars[cursor].is_whitespace() {
        cursor += 1;
    }
    while cursor < chars.len() && chars[cursor].is_whitespace() {
        cursor += 1;
    }
    cursor
}

fn read_plain(prompt: &str, history: &[String]) -> Result<String> {
    let mut out = std::io::stdout();
    write!(out, "{prompt}")?;
    out.flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> Result<String> {
        let mut chars: Vec<char> = Vec::new();
        let mut cursor = 0usize;
        let mut history_idx = history.len();
        let redraw = |out: &mut std::io::Stdout, chars: &[char], cursor: usize| -> Result<()> {
            let line: String = chars.iter().collect();
            let display = render_value(&line);
            write!(out, "\r\x1b[2K{prompt}{display}")?;
            let right = chars.len().saturating_sub(cursor);
            if right > 0 {
                write!(out, "\x1b[{right}D")?;
            }
            out.flush()?;
            Ok(())
        };
        loop {
            if let Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) = crossterm::event::read()?
            {
                if kind == KeyEventKind::Release {
                    continue;
                }
                match code {
                    KeyCode::Enter => {
                        writeln!(out)?;
                        out.flush()?;
                        return Ok(chars.iter().collect());
                    }
                    KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                        chars.insert(cursor, '\n');
                        cursor += 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Backspace if cursor > 0 => {
                        chars.remove(cursor - 1);
                        cursor -= 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        writeln!(out)?;
                        out.flush()?;
                        crossterm::terminal::disable_raw_mode().ok();
                        std::process::exit(0);
                    }
                    KeyCode::Left if modifiers.contains(KeyModifiers::ALT) => {
                        cursor = prev_word(&chars, cursor);
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Right if modifiers.contains(KeyModifiers::ALT) => {
                        cursor = next_word(&chars, cursor);
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Left if cursor > 0 => {
                        cursor -= 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Right if cursor < chars.len() => {
                        cursor += 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Up if !history.is_empty() => {
                        history_idx = history_idx.saturating_sub(1);
                        chars = history[history_idx].chars().collect();
                        cursor = chars.len();
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Down if !history.is_empty() => {
                        if history_idx + 1 < history.len() {
                            history_idx += 1;
                            chars = history[history_idx].chars().collect();
                        } else {
                            history_idx = history.len();
                            chars.clear();
                        }
                        cursor = chars.len();
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Char(c) => {
                        chars.insert(cursor, c);
                        cursor += 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    _ => {}
                }
            }
        }
    })();
    crossterm::terminal::disable_raw_mode()?;
    result
}

fn read_bordered(history: &[String]) -> Result<String> {
    let (cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let width = (cols.max(1)) as usize;
    let bar = "─".repeat(width);
    let border = format!("{GRAY}{bar}{RESET}");
    let mut out = std::io::stdout();
    let mut chars: Vec<char> = Vec::new();
    let mut cursor = 0usize;
    let mut history_idx = history.len();

    write!(out, "\n{border}\n")?;
    writeln!(out, "› ")?;
    write!(out, "{border}\x1b[1A\r\x1b[3G")?;
    out.flush()?;

    crossterm::terminal::enable_raw_mode()?;
    let result = (|| -> Result<String> {
        let redraw = |out: &mut std::io::Stdout, chars: &[char], cursor: usize| -> Result<()> {
            let line: String = chars.iter().collect();
            let display = render_value(&line);
            write!(out, "\r\x1b[2K› {display}")?;
            let right = chars.len().saturating_sub(cursor);
            if right > 0 {
                write!(out, "\x1b[{right}D")?;
            }
            out.flush()?;
            Ok(())
        };
        loop {
            if let Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) = crossterm::event::read()?
            {
                if kind == KeyEventKind::Release {
                    continue;
                }
                match code {
                    KeyCode::Enter => {
                        if chars.is_empty() {
                            write!(out, "\x1b[1A\x1b[2K\x1b[1A\x1b[2K\r")?;
                        } else {
                            write!(out, "\x1b[1B\x1b[2K\r")?;
                        }
                        out.flush()?;
                        return Ok(chars.iter().collect());
                    }
                    KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                        chars.insert(cursor, '\n');
                        cursor += 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Backspace if cursor > 0 => {
                        chars.remove(cursor - 1);
                        cursor -= 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        writeln!(out, "{RESET}")?;
                        out.flush()?;
                        crossterm::terminal::disable_raw_mode().ok();
                        std::process::exit(0);
                    }
                    KeyCode::Left if modifiers.contains(KeyModifiers::ALT) => {
                        cursor = prev_word(&chars, cursor);
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Right if modifiers.contains(KeyModifiers::ALT) => {
                        cursor = next_word(&chars, cursor);
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Left if cursor > 0 => {
                        cursor -= 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Right if cursor < chars.len() => {
                        cursor += 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Up if !history.is_empty() => {
                        history_idx = history_idx.saturating_sub(1);
                        chars = history[history_idx].chars().collect();
                        cursor = chars.len();
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Down if !history.is_empty() => {
                        if history_idx + 1 < history.len() {
                            history_idx += 1;
                            chars = history[history_idx].chars().collect();
                        } else {
                            history_idx = history.len();
                            chars.clear();
                        }
                        cursor = chars.len();
                        redraw(&mut out, &chars, cursor)?;
                    }
                    KeyCode::Char(c) => {
                        chars.insert(cursor, c);
                        cursor += 1;
                        redraw(&mut out, &chars, cursor)?;
                    }
                    _ => {}
                }
            }
        }
    })();
    crossterm::terminal::disable_raw_mode()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_persists_jsonl_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut history = InputHistory::load(path.display().to_string(), 2, true);
        history.push("one").unwrap();
        history.push("two").unwrap();
        history.push("three").unwrap();
        let history = InputHistory::load(path.display().to_string(), 2, true);
        assert_eq!(history.entries(), &["two".to_string(), "three".to_string()]);
    }

    #[test]
    fn word_movement_skips_whitespace() {
        let chars: Vec<char> = "one two".chars().collect();
        assert_eq!(prev_word(&chars, chars.len()), 4);
        assert_eq!(next_word(&chars, 0), 4);
    }
}

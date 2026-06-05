use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::theme::{ACCENT, BOLD, MUTED, RESET};

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

pub async fn plain_read_line(prompt: String) -> Result<String> {
    plain_read_line_with_history(prompt, Vec::new(), Vec::new()).await
}

/// `commands` are `(name, description)` slash-commands offered as completions
/// (empty for sub-prompts that don't want completion).
pub async fn plain_read_line_with_history(
    prompt: String,
    history: Vec<String>,
    commands: Vec<(String, String)>,
) -> Result<String> {
    tokio::task::spawn_blocking(move || read_plain(&prompt, &history, &commands)).await?
}

fn render_value(value: &str) -> String {
    value.replace('\n', "⏎")
}

/// Maximum number of command rows shown in the completion menu at once.
const MENU_MAX_ROWS: usize = 8;

/// Visible width of a string, ignoring ANSI escape sequences (`ESC [ … m`).
fn visible_width(s: &str) -> usize {
    let mut n = 0;
    let mut in_esc = false;
    for ch in s.chars() {
        if in_esc {
            if ch == 'm' {
                in_esc = false;
            }
        } else if ch == '\x1b' {
            in_esc = true;
        } else {
            n += 1;
        }
    }
    n
}

/// Slash-commands the current line is a prefix of, for the completion menu.
/// Empty when: not a `/`-line, the cursor isn't at the end, completion was
/// dismissed, or the only match is exactly what's already typed.
fn completion_matches<'a>(
    line: &str,
    cursor: usize,
    len: usize,
    commands: &'a [(String, String)],
    dismissed: bool,
) -> Vec<&'a (String, String)> {
    if dismissed || cursor != len || !line.starts_with('/') {
        return Vec::new();
    }
    let matches: Vec<&(String, String)> = commands
        .iter()
        .filter(|(n, _)| n.starts_with(line))
        .collect();
    if matches.len() == 1 && matches[0].0 == line {
        return Vec::new();
    }
    matches
}

/// Build the full redraw string for the input line plus (optionally) the
/// completion menu, leaving the cursor parked at the logical edit position.
///
/// Sequence: clear the input line and everything below it, draw the prompt +
/// text + dim ghost (the selected match's remainder), then — if there are
/// matches — draw the menu on the lines beneath and move the cursor back up to
/// the input line. Pure (returns the bytes to write) so it can be unit-tested.
#[allow(clippy::too_many_arguments)]
fn render_input(
    prompt: &str,
    prompt_cols: usize,
    chars: &[char],
    cursor: usize,
    commands: &[(String, String)],
    sel: usize,
    dismissed: bool,
    term_cols: usize,
) -> String {
    let line: String = chars.iter().collect();
    let display = render_value(&line);
    let matches = completion_matches(&line, cursor, chars.len(), commands, dismissed);
    let sel = if matches.is_empty() {
        0
    } else {
        sel.min(matches.len() - 1)
    };
    let ghost = matches
        .get(sel)
        .and_then(|(n, _)| n.strip_prefix(line.as_str()))
        .filter(|r| !r.is_empty())
        .unwrap_or("")
        .to_string();

    let mut s = String::new();
    // Clear current line + everything below (removes a previously drawn menu).
    s.push_str("\r\x1b[0J");
    s.push_str(prompt);
    s.push_str(&display);
    if !ghost.is_empty() {
        s.push_str(MUTED);
        s.push_str(&ghost);
        s.push_str(RESET);
    }

    if matches.is_empty() {
        // No menu: park the cursor at the logical position.
        let back = ghost.chars().count() + chars.len().saturating_sub(cursor);
        if back > 0 {
            s.push_str(&format!("\x1b[{back}D"));
        }
        return s;
    }

    // Draw the menu beneath the input line.
    let name_w = matches
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(8)
        .min(18);
    let shown = matches.len().min(MENU_MAX_ROWS);
    let mut rows = 0;
    for (i, (name, desc)) in matches.iter().take(shown).enumerate() {
        s.push_str("\r\n");
        rows += 1;
        // Leave room for: 2 gutter + 2 marker + name_w + 2 gap.
        let desc_room = term_cols.saturating_sub(6 + name_w);
        let desc = truncate(desc, desc_room);
        if i == sel {
            s.push_str(&format!(
                "  {ACCENT}▸ {BOLD}{name:<name_w$}{RESET}  {MUTED}{desc}{RESET}"
            ));
        } else {
            s.push_str(&format!("    {name:<name_w$}  {MUTED}{desc}{RESET}"));
        }
    }
    if matches.len() > shown {
        s.push_str(&format!(
            "\r\n  {MUTED}… +{} more{RESET}",
            matches.len() - shown
        ));
        rows += 1;
    }
    // Move cursor back up to the input line, then to the logical column.
    s.push_str(&format!("\x1b[{rows}A\r"));
    let col = prompt_cols + cursor;
    if col > 0 {
        s.push_str(&format!("\x1b[{col}C"));
    }
    s
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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

fn read_plain(prompt: &str, history: &[String], commands: &[(String, String)]) -> Result<String> {
    let mut out = std::io::stdout();
    write!(out, "{prompt}")?;
    out.flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let prompt_cols = visible_width(prompt);
    let term_cols = crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80);

    let result = (|| -> Result<String> {
        let mut chars: Vec<char> = Vec::new();
        let mut cursor = 0usize;
        let mut history_idx = history.len();
        // Completion-menu state: which row is selected, and whether the menu was
        // dismissed (Esc) until the next edit.
        let mut sel = 0usize;
        let mut dismissed = false;

        let redraw = |out: &mut std::io::Stdout,
                      chars: &[char],
                      cursor: usize,
                      sel: usize,
                      dismissed: bool|
         -> Result<()> {
            let s = render_input(
                prompt,
                prompt_cols,
                chars,
                cursor,
                commands,
                sel,
                dismissed,
                term_cols,
            );
            write!(out, "{s}")?;
            out.flush()?;
            Ok(())
        };
        // Number of completion matches for the current edit state (0 = no menu).
        let match_count = |chars: &[char], cursor: usize, dismissed: bool| -> usize {
            let line: String = chars.iter().collect();
            completion_matches(&line, cursor, chars.len(), commands, dismissed).len()
        };
        // Name of the currently selected completion, if the menu is open.
        let selected_name =
            |chars: &[char], cursor: usize, sel: usize, dismissed: bool| -> Option<String> {
                let line: String = chars.iter().collect();
                let m = completion_matches(&line, cursor, chars.len(), commands, dismissed);
                if m.is_empty() {
                    None
                } else {
                    Some(m[sel.min(m.len() - 1)].0.clone())
                }
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
                        // Clear any open menu, then drop to the next line.
                        redraw(&mut out, &chars, cursor, sel, true)?;
                        writeln!(out)?;
                        out.flush()?;
                        return Ok(chars.iter().collect());
                    }
                    KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                        chars.insert(cursor, '\n');
                        cursor += 1;
                        sel = 0;
                        dismissed = false;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        redraw(&mut out, &chars, cursor, sel, true)?;
                        writeln!(out)?;
                        out.flush()?;
                        crossterm::terminal::disable_raw_mode().ok();
                        std::process::exit(0);
                    }
                    KeyCode::Esc => {
                        dismissed = true;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Backspace if cursor > 0 => {
                        chars.remove(cursor - 1);
                        cursor -= 1;
                        sel = 0;
                        dismissed = false;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Left if modifiers.contains(KeyModifiers::ALT) => {
                        cursor = prev_word(&chars, cursor);
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Right if modifiers.contains(KeyModifiers::ALT) => {
                        cursor = next_word(&chars, cursor);
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Left if cursor > 0 => {
                        cursor -= 1;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Right if cursor < chars.len() => {
                        cursor += 1;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    // Tab accepts the selected completion (+ trailing space, ready
                    // for args). Right at end-of-line accepts it without the space.
                    KeyCode::Tab => {
                        if let Some(name) = selected_name(&chars, cursor, sel, dismissed) {
                            chars = name.chars().collect();
                            chars.push(' ');
                            cursor = chars.len();
                            sel = 0;
                            dismissed = false;
                            redraw(&mut out, &chars, cursor, sel, dismissed)?;
                        }
                    }
                    KeyCode::Right => {
                        if let Some(name) = selected_name(&chars, cursor, sel, dismissed) {
                            chars = name.chars().collect();
                            cursor = chars.len();
                            sel = 0;
                            dismissed = false;
                            redraw(&mut out, &chars, cursor, sel, dismissed)?;
                        }
                    }
                    // Up/Down navigate the menu when it's open, else the history.
                    KeyCode::Up if match_count(&chars, cursor, dismissed) > 0 => {
                        sel = sel.saturating_sub(1);
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Down if match_count(&chars, cursor, dismissed) > 0 => {
                        let n = match_count(&chars, cursor, dismissed);
                        sel = (sel + 1).min(n - 1);
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Up if !history.is_empty() => {
                        history_idx = history_idx.saturating_sub(1);
                        chars = history[history_idx].chars().collect();
                        cursor = chars.len();
                        sel = 0;
                        dismissed = false;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
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
                        sel = 0;
                        dismissed = false;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
                    }
                    KeyCode::Char(c) => {
                        chars.insert(cursor, c);
                        cursor += 1;
                        sel = 0;
                        dismissed = false;
                        redraw(&mut out, &chars, cursor, sel, dismissed)?;
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

    fn cmds() -> Vec<(String, String)> {
        vec![
            ("/compact".into(), "compact".into()),
            ("/compare".into(), "compare".into()),
            ("/config".into(), "config".into()),
            ("/help".into(), "help".into()),
        ]
    }

    #[test]
    fn matches_only_for_slash_prefix_at_end() {
        let c = cmds();
        assert_eq!(completion_matches("/co", 3, 3, &c, false).len(), 3);
        // not a slash command
        assert!(completion_matches("co", 2, 2, &c, false).is_empty());
        // cursor not at end → no menu (don't fight mid-line editing)
        assert!(completion_matches("/co", 1, 3, &c, false).is_empty());
        // dismissed (Esc)
        assert!(completion_matches("/co", 3, 3, &c, true).is_empty());
        // exact unique match → already complete, no menu
        assert!(completion_matches("/help", 5, 5, &c, false).is_empty());
        // no matches
        assert!(completion_matches("/zzz", 4, 4, &c, false).is_empty());
    }

    #[test]
    fn render_shows_selected_ghost_and_menu_rows() {
        let chars: Vec<char> = "/co".chars().collect();
        let out = render_input("> ", 2, &chars, chars.len(), &cmds(), 1, false, 80);
        // Selected row is index 1 (/compare) → ghost is its remainder "mpare".
        assert!(out.contains("mpare"), "ghost of selected match: {out:?}");
        // All three matches appear as menu rows.
        for name in ["/compact", "/compare", "/config"] {
            assert!(out.contains(name), "menu row {name} missing: {out:?}");
        }
        // The selected row is marked with the accent pointer.
        assert!(out.contains("▸"), "selected marker missing: {out:?}");
        // It clears below and restores the cursor up onto the input line.
        assert!(out.starts_with("\r\x1b[0J"));
        assert!(
            out.contains("\x1b[3A"),
            "cursor moves back up 3 rows: {out:?}"
        );
    }

    #[test]
    fn render_no_menu_when_no_matches() {
        let chars: Vec<char> = "hello".chars().collect();
        let out = render_input("> ", 2, &chars, chars.len(), &cmds(), 0, false, 80);
        assert!(!out.contains('▸'));
        assert!(!out.contains("\r\n"));
    }

    #[test]
    fn visible_width_ignores_ansi() {
        assert_eq!(visible_width("  \x1b[96m❯\x1b[0m "), 4);
        assert_eq!(visible_width("abc"), 3);
    }

    #[test]
    fn word_movement_skips_whitespace() {
        let chars: Vec<char> = "one two".chars().collect();
        assert_eq!(prev_word(&chars, chars.len()), 4);
        assert_eq!(next_word(&chars, 0), 4);
    }
}

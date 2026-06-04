//! Centralized TUI theme: a small high-contrast palette plus layout helpers so
//! every surface (banner, input, per-turn renderer, status line) looks
//! consistent.
//!
//! Readability note: we deliberately avoid ANSI "faint" (`\x1b[2m`). Faint
//! renders as washed-out, hard-to-read text on many terminals and themes — it
//! was the main reason the old TUI looked dim. Secondary text uses bright-black
//! (`MUTED`) instead, which stays legible on both light and dark backgrounds.

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

/// Accent — prompts, headers, the active step. Bright cyan.
pub const ACCENT: &str = "\x1b[96m";
/// A slightly deeper accent for large fills (the logo).
pub const ACCENT_DEEP: &str = "\x1b[36m";
/// Primary content: the terminal's default foreground (max contrast).
pub const TEXT: &str = "\x1b[0m";
/// Secondary text — labels, summaries, hints. Bright-black, NOT faint.
pub const MUTED: &str = "\x1b[90m";
pub const SUCCESS: &str = "\x1b[92m";
pub const WARN: &str = "\x1b[93m";
pub const ERROR: &str = "\x1b[91m";

/// Left margin every block shares, so the transcript has a consistent gutter.
pub const PAD: &str = "  ";

/// The real terminal width in columns (clamped to a sane range). Unlike a fixed
/// cap, this lets wrapped content fill the window the way naturally-wrapped
/// terminal text does.
pub fn cols() -> usize {
    crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
        .clamp(20, 400)
}

/// Usable width for wrapped body text: the terminal minus the left gutter and a
/// one-column right breathing margin.
pub fn content_width() -> usize {
    cols().saturating_sub(PAD.len() + 1).max(20)
}

/// A muted full-width horizontal rule, indented by `PAD`.
pub fn rule() -> String {
    let dashes = cols().saturating_sub(PAD.len());
    format!("{PAD}{MUTED}{}{RESET}", "─".repeat(dashes))
}

/// A turn header: an accent label followed by a short rule (~20% of the width)
/// that fades from bright cyan to dark, e.g. `response ──────╴`. The fade is
/// done per-character with 256-color codes so it reads as a soft taper rather
/// than a hard-edged bar. No bottom or side borders — just this top accent.
pub fn fade_header(label: &str) -> String {
    // 256-color ramp: bright cyan → teal → dark gray (fades toward a dark bg).
    const RAMP: [u8; 12] = [51, 45, 39, 38, 37, 31, 30, 24, 23, 237, 235, 234];
    let len = (cols() / 5).clamp(6, 30);
    let last = RAMP.len() - 1;
    let denom = len.saturating_sub(1).max(1);
    let mut fade = String::new();
    for i in 0..len {
        let idx = ((i * last) / denom).min(last);
        fade.push_str(&format!("\x1b[38;5;{}m─", RAMP[idx]));
    }
    format!("{PAD}{ACCENT}{BOLD}{label}{RESET} {fade}{RESET}")
}

//! Centralized TUI theme: a small high-contrast palette plus box-drawing
//! helpers, so every surface (banner, input frame, per-turn renderer, status
//! line) looks consistent.
//!
//! Readability note: we deliberately avoid ANSI "faint" (`\x1b[2m`). Faint
//! renders as washed-out, hard-to-read text on many terminals and themes —
//! it was the main reason the old TUI looked dim. Secondary text uses
//! bright-black (`MUTED`) instead, which stays legible on both light and dark
//! backgrounds.

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

/// Accent — prompts, panel borders, headers, the active step. Bright cyan.
pub const ACCENT: &str = "\x1b[96m";
/// A slightly deeper accent for large fills (the logo, long rules).
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

/// Terminal width in columns, clamped so panels stay tidy on very wide or
/// very narrow terminals.
pub fn width() -> usize {
    crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
        .clamp(40, 100)
}

/// A muted full-width horizontal rule, indented by `PAD`.
pub fn rule() -> String {
    let dashes = width().saturating_sub(PAD.len());
    format!("{PAD}{MUTED}{}{RESET}", "─".repeat(dashes))
}

/// Rounded panel top with an inline accent label:
/// `╭─ label ───────────────╮`
pub fn panel_top(label: &str) -> String {
    let inner = width().saturating_sub(PAD.len());
    // ╭ + "─ " + label + " " + fill + ╮
    let used = 1 + 2 + label.chars().count() + 1 + 1;
    let fill = inner.saturating_sub(used);
    format!(
        "{PAD}{ACCENT}╭─ {BOLD}{label}{RESET}{ACCENT} {}╮{RESET}",
        "─".repeat(fill)
    )
}

/// Rounded panel bottom matching [`panel_top`]: `╰────────────────────╯`
pub fn panel_bottom() -> String {
    let inner = width().saturating_sub(PAD.len());
    let fill = inner.saturating_sub(2);
    format!("{PAD}{ACCENT}╰{}╯{RESET}", "─".repeat(fill))
}

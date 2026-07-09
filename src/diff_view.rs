//! Colored rendering for unified diffs produced by [`crate::tools::diff::unified_diff`].
//!
//! Classification is order-sensitive: file headers (`---`/`+++`) must be
//! checked before the single-character `-`/`+` line markers, since a header
//! line also starts with those characters.

use crate::theme::{ACCENT, ERROR, MUTED, RESET, SUCCESS};

/// Apply theme colors to a single diff line based on its unified-diff prefix.
/// Pure and side-effect-free so classification is directly unit-testable.
pub fn colorize_diff_line(line: &str) -> String {
    if line.starts_with("+++") || line.starts_with("---") {
        format!("{MUTED}{line}{RESET}")
    } else if line.starts_with("@@") {
        format!("{ACCENT}{line}{RESET}")
    } else if let Some(rest) = line.strip_prefix('+') {
        format!("{SUCCESS}+{rest}{RESET}")
    } else if let Some(rest) = line.strip_prefix('-') {
        format!("{ERROR}-{rest}{RESET}")
    } else {
        format!("{MUTED}{line}{RESET}")
    }
}

/// Build the colored, gutter-indented, capped diff text (pure — testable
/// without capturing stdout).
fn render_diff(diff: &str, max_lines: usize) -> String {
    let mut out = String::new();
    for line in diff.lines().take(max_lines) {
        out.push_str("  ");
        out.push_str(&colorize_diff_line(line));
        out.push('\n');
    }
    if diff.lines().count() > max_lines {
        out.push_str(&format!("  {MUTED}…diff truncated for display{RESET}\n"));
    }
    out
}

/// Print a unified diff with per-line coloring, indented by the caller's
/// gutter, capped at `max_lines` with a truncation notice.
pub fn print_diff(diff: &str, max_lines: usize) {
    print!("{}", render_diff(diff, max_lines));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_added_line() {
        let out = colorize_diff_line("+hello");
        assert!(out.contains('+'));
        assert!(out.contains("hello"));
        assert!(out.starts_with(&SUCCESS.to_string()));
    }

    #[test]
    fn classifies_removed_line() {
        let out = colorize_diff_line("-hello");
        assert!(out.starts_with(&ERROR.to_string()));
    }

    #[test]
    fn classifies_hunk_header() {
        let out = colorize_diff_line("@@ -1 +1 @@");
        assert!(out.starts_with(&ACCENT.to_string()));
    }

    #[test]
    fn file_headers_are_not_treated_as_added_or_removed() {
        let plus_header = colorize_diff_line("+++ path.rs");
        let minus_header = colorize_diff_line("--- path.rs");
        assert!(plus_header.starts_with(&MUTED.to_string()));
        assert!(minus_header.starts_with(&MUTED.to_string()));
        assert!(!plus_header.starts_with(&SUCCESS.to_string()));
        assert!(!minus_header.starts_with(&ERROR.to_string()));
    }

    #[test]
    fn plain_context_line_is_muted() {
        let out = colorize_diff_line("unchanged context");
        assert!(out.starts_with(&MUTED.to_string()));
    }

    #[test]
    fn truncation_notice_appears_past_cap() {
        let diff = (0..5)
            .map(|i| format!("+line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let rendered = render_diff(&diff, 3);
        assert_eq!(rendered.lines().count(), 4); // 3 diff lines + notice
        assert!(rendered.contains("truncated for display"));
        assert!(!rendered.contains("line3")); // beyond the cap
    }

    #[test]
    fn no_truncation_notice_when_under_cap() {
        let diff = "+only line";
        let rendered = render_diff(diff, 80);
        assert!(!rendered.contains("truncated"));
    }
}

use anyhow::{anyhow, Result};
use chrono::Utc;
use std::path::Path;
use std::process::Command;

use crate::project_memory::ProjectIndexFreshness;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitFileKind {
    Tracked,
    Renamed,
    Untracked,
    Conflict,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileState {
    pub path: String,
    pub original_path: Option<String>,
    pub staged: Option<char>,
    pub unstaged: Option<char>,
    pub kind: GitFileKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitBranchState {
    pub oid: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i32,
    pub behind: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipcheckSnapshot {
    pub workspace_root: String,
    pub git_root: String,
    pub branch: GitBranchState,
    pub files: Vec<GitFileState>,
    pub staged_diff_stat: String,
    pub unstaged_diff_stat: String,
}

impl ShipcheckSnapshot {
    pub fn is_clean(&self) -> bool {
        self.files.is_empty()
    }

    pub fn staged_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| status_changed(file.staged))
            .count()
    }

    pub fn unstaged_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| status_changed(file.unstaged))
            .count()
    }

    pub fn untracked_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.kind == GitFileKind::Untracked)
            .count()
    }

    pub fn conflict_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.kind == GitFileKind::Conflict)
            .count()
    }

    pub fn ignored_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.kind == GitFileKind::Ignored)
            .count()
    }

    pub fn branch_label(&self) -> String {
        let head = self.branch.head.as_deref().unwrap_or("(unknown)");
        match self.branch.upstream.as_deref() {
            Some(upstream) => format!(
                "{head} -> {upstream} (+{}/-{})",
                self.branch.ahead, self.branch.behind
            ),
            None => head.to_string(),
        }
    }
}

pub fn collect_shipcheck(workspace_root: &str) -> Result<ShipcheckSnapshot> {
    let git_root = run_git(workspace_root, &["rev-parse", "--show-toplevel"])?
        .trim()
        .to_string();
    let status = run_git(workspace_root, &["status", "--porcelain=v2", "--branch"])?;
    let (branch, files) = parse_status_porcelain_v2(&status)?;
    let staged_diff_stat = run_git(workspace_root, &["diff", "--cached", "--stat", "--"])?;
    let unstaged_diff_stat = run_git(workspace_root, &["diff", "--stat", "--"])?;

    Ok(ShipcheckSnapshot {
        workspace_root: workspace_root.to_string(),
        git_root,
        branch,
        files,
        staged_diff_stat: staged_diff_stat.trim().to_string(),
        unstaged_diff_stat: unstaged_diff_stat.trim().to_string(),
    })
}

fn run_git(workspace_root: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .map_err(|e| anyhow!("failed to run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(anyhow!(
            "git {} failed{}",
            args.join(" "),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_status_porcelain_v2(input: &str) -> Result<(GitBranchState, Vec<GitFileState>)> {
    let mut branch = GitBranchState::default();
    let mut files = Vec::new();

    for line in input.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            parse_branch_line(rest, &mut branch);
            continue;
        }
        if let Some(file) = parse_file_line(line)? {
            files.push(file);
        }
    }

    Ok((branch, files))
}

fn parse_branch_line(line: &str, branch: &mut GitBranchState) {
    if let Some(value) = line.strip_prefix("branch.oid ") {
        if value != "(initial)" {
            branch.oid = Some(value.to_string());
        }
    } else if let Some(value) = line.strip_prefix("branch.head ") {
        branch.head = Some(value.to_string());
    } else if let Some(value) = line.strip_prefix("branch.upstream ") {
        branch.upstream = Some(value.to_string());
    } else if let Some(value) = line.strip_prefix("branch.ab ") {
        for part in value.split_whitespace() {
            if let Some(n) = part.strip_prefix('+').and_then(|s| s.parse::<i32>().ok()) {
                branch.ahead = n;
            } else if let Some(n) = part.strip_prefix('-').and_then(|s| s.parse::<i32>().ok()) {
                branch.behind = n;
            }
        }
    }
}

fn parse_file_line(line: &str) -> Result<Option<GitFileState>> {
    if let Some(path) = line.strip_prefix("? ") {
        return Ok(Some(GitFileState {
            path: path.to_string(),
            original_path: None,
            staged: None,
            unstaged: None,
            kind: GitFileKind::Untracked,
        }));
    }
    if let Some(path) = line.strip_prefix("! ") {
        return Ok(Some(GitFileState {
            path: path.to_string(),
            original_path: None,
            staged: None,
            unstaged: None,
            kind: GitFileKind::Ignored,
        }));
    }
    if line.starts_with("1 ") {
        let parts: Vec<&str> = line.splitn(9, ' ').collect();
        if parts.len() != 9 {
            return Err(anyhow!("malformed ordinary git status line: {line}"));
        }
        let (staged, unstaged) = parse_xy(parts[1]);
        return Ok(Some(GitFileState {
            path: parts[8].to_string(),
            original_path: None,
            staged,
            unstaged,
            kind: GitFileKind::Tracked,
        }));
    }
    if line.starts_with("2 ") {
        let parts: Vec<&str> = line.splitn(10, ' ').collect();
        if parts.len() != 10 {
            return Err(anyhow!("malformed rename/copy git status line: {line}"));
        }
        let (path, original_path) = parts[9]
            .split_once('\t')
            .map(|(path, original)| (path.to_string(), Some(original.to_string())))
            .unwrap_or_else(|| (parts[9].to_string(), None));
        let (staged, unstaged) = parse_xy(parts[1]);
        return Ok(Some(GitFileState {
            path,
            original_path,
            staged,
            unstaged,
            kind: GitFileKind::Renamed,
        }));
    }
    if line.starts_with("u ") {
        let parts: Vec<&str> = line.splitn(11, ' ').collect();
        if parts.len() != 11 {
            return Err(anyhow!("malformed unmerged git status line: {line}"));
        }
        let (staged, unstaged) = parse_xy(parts[1]);
        return Ok(Some(GitFileState {
            path: parts[10].to_string(),
            original_path: None,
            staged,
            unstaged,
            kind: GitFileKind::Conflict,
        }));
    }
    Ok(None)
}

fn parse_xy(xy: &str) -> (Option<char>, Option<char>) {
    let mut chars = xy.chars();
    (chars.next(), chars.next())
}

fn status_changed(status: Option<char>) -> bool {
    matches!(status, Some(c) if c != '.' && c != ' ')
}

pub fn render_markdown(
    snapshot: &ShipcheckSnapshot,
    freshness: Option<&ProjectIndexFreshness>,
) -> String {
    let mut out = String::new();
    out.push_str("# Small Harness Shipcheck\n\n");
    out.push_str(&format!("Generated: {}\n\n", Utc::now().to_rfc3339()));
    out.push_str("## Git\n\n");
    out.push_str(&format!("- Workspace: `{}`\n", snapshot.workspace_root));
    out.push_str(&format!("- Git root: `{}`\n", snapshot.git_root));
    out.push_str(&format!("- Branch: `{}`\n", snapshot.branch_label()));
    out.push_str(&format!(
        "- Status: {}\n\n",
        if snapshot.is_clean() {
            "clean"
        } else {
            "dirty"
        }
    ));
    out.push_str("## Working Tree\n\n");
    out.push_str(&format!("- Staged files: {}\n", snapshot.staged_count()));
    out.push_str(&format!(
        "- Unstaged files: {}\n",
        snapshot.unstaged_count()
    ));
    out.push_str(&format!(
        "- Untracked files: {}\n",
        snapshot.untracked_count()
    ));
    out.push_str(&format!("- Conflicts: {}\n", snapshot.conflict_count()));
    if snapshot.ignored_count() > 0 {
        out.push_str(&format!("- Ignored files: {}\n", snapshot.ignored_count()));
    }
    out.push('\n');

    if !snapshot.files.is_empty() {
        out.push_str("## Files\n\n");
        for file in &snapshot.files {
            out.push_str(&format!("- `{}`", file.path));
            if let Some(original) = &file.original_path {
                out.push_str(&format!(" from `{original}`"));
            }
            out.push_str(&format!(" ({})\n", file_status_label(file)));
        }
        out.push('\n');
    }

    push_diff_stat(&mut out, "Staged Diff", &snapshot.staged_diff_stat);
    push_diff_stat(&mut out, "Unstaged Diff", &snapshot.unstaged_diff_stat);

    out.push_str("## Project Memory\n\n");
    match freshness {
        Some(report) if report.indexed_files > 0 || report.workspace_files > 0 => {
            out.push_str(&format!(
                "- Indexed files: {}\n- Workspace files: {}\n- Fresh: {}\n- Stale: {}\n- Missing: {}\n- Deleted: {}\n- Read errors: {}\n",
                report.indexed_files,
                report.workspace_files,
                report.fresh,
                report.stale,
                report.missing,
                report.deleted,
                report.read_errors
            ));
        }
        Some(_) => out.push_str("- No project-memory index found.\n"),
        None => out.push_str("- Project memory disabled.\n"),
    }

    out
}

fn push_diff_stat(out: &mut String, title: &str, stat: &str) {
    out.push_str(&format!("## {title}\n\n"));
    if stat.trim().is_empty() {
        out.push_str("No changes.\n\n");
    } else {
        out.push_str("```text\n");
        out.push_str(stat);
        out.push_str("\n```\n\n");
    }
}

pub fn file_status_label(file: &GitFileState) -> String {
    match file.kind {
        GitFileKind::Untracked => "untracked".to_string(),
        GitFileKind::Ignored => "ignored".to_string(),
        GitFileKind::Conflict => "conflict".to_string(),
        GitFileKind::Tracked | GitFileKind::Renamed => {
            let mut parts = Vec::new();
            if status_changed(file.staged) {
                parts.push(format!("staged {}", file.staged.unwrap_or('?')));
            }
            if status_changed(file.unstaged) {
                parts.push(format!("unstaged {}", file.unstaged.unwrap_or('?')));
            }
            if parts.is_empty() {
                "tracked".to_string()
            } else {
                parts.join(", ")
            }
        }
    }
}

pub fn default_export_path(session_dir: &str) -> std::path::PathBuf {
    Path::new(session_dir).join("shipcheck").join(format!(
        "{}.md",
        Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn parses_clean_branch_status() {
        let input = "\
# branch.oid abc123
# branch.head main
# branch.upstream origin/main
# branch.ab +0 -0
";
        let (branch, files) = parse_status_porcelain_v2(input).unwrap();

        assert_eq!(branch.oid.as_deref(), Some("abc123"));
        assert_eq!(branch.head.as_deref(), Some("main"));
        assert_eq!(branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!(branch.ahead, 0);
        assert_eq!(branch.behind, 0);
        assert!(files.is_empty());
    }

    #[test]
    fn parses_dirty_counts_and_renames() {
        let input = "\
# branch.oid abc123
# branch.head feature
# branch.upstream origin/feature
# branch.ab +2 -1
1 M. N... 100644 100644 100644 abc def src/staged.rs
1 .M N... 100644 100644 100644 abc def README copy.md
1 MM N... 100644 100644 100644 abc def src/both.rs
2 R. N... 100644 100644 100644 abc def R100 src/new.rs\tsrc/old.rs
u UU N... 100644 100644 100644 100644 a b c src/conflict.rs
? notes/today.md
";
        let (branch, files) = parse_status_porcelain_v2(input).unwrap();
        let snapshot = ShipcheckSnapshot {
            workspace_root: ".".to_string(),
            git_root: ".".to_string(),
            branch,
            files,
            staged_diff_stat: String::new(),
            unstaged_diff_stat: String::new(),
        };

        assert_eq!(snapshot.branch.ahead, 2);
        assert_eq!(snapshot.branch.behind, 1);
        assert_eq!(snapshot.staged_count(), 4);
        assert_eq!(snapshot.unstaged_count(), 3);
        assert_eq!(snapshot.untracked_count(), 1);
        assert_eq!(snapshot.conflict_count(), 1);
        let renamed = snapshot
            .files
            .iter()
            .find(|file| file.path == "src/new.rs")
            .unwrap();
        assert_eq!(renamed.original_path.as_deref(), Some("src/old.rs"));
    }

    #[test]
    fn renders_markdown_report() {
        let snapshot = ShipcheckSnapshot {
            workspace_root: "/repo".to_string(),
            git_root: "/repo".to_string(),
            branch: GitBranchState {
                head: Some("main".to_string()),
                upstream: Some("origin/main".to_string()),
                ahead: 1,
                behind: 0,
                oid: Some("abc123".to_string()),
            },
            files: vec![GitFileState {
                path: "src/main.rs".to_string(),
                original_path: None,
                staged: Some('M'),
                unstaged: Some('.'),
                kind: GitFileKind::Tracked,
            }],
            staged_diff_stat: " src/main.rs | 1 +".to_string(),
            unstaged_diff_stat: String::new(),
        };
        let freshness = ProjectIndexFreshness {
            indexed_files: 1,
            workspace_files: 1,
            fresh: 1,
            ..Default::default()
        };
        let md = render_markdown(&snapshot, Some(&freshness));

        assert!(md.contains("# Small Harness Shipcheck"));
        assert!(md.contains("main -> origin/main (+1/-0)"));
        assert!(md.contains("- Staged files: 1"));
        assert!(md.contains("src/main.rs | 1 +"));
        assert!(md.contains("- Fresh: 1"));
    }

    #[test]
    fn collects_real_git_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let init = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .arg("init")
            .output()
            .unwrap();
        assert!(init.status.success());
        fs::write(dir.path().join("notes.md"), "ship it\n").unwrap();

        let snapshot = collect_shipcheck(dir.path().to_str().unwrap()).unwrap();

        assert_eq!(
            std::path::PathBuf::from(&snapshot.git_root),
            fs::canonicalize(dir.path()).unwrap()
        );
        assert_eq!(snapshot.untracked_count(), 1);
        assert!(!snapshot.is_clean());
    }
}

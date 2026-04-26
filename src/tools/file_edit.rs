use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{diff::unified_diff, PathPolicy, Tool, ToolPreview};

pub struct FileEditTool {
    pub approve: bool,
    pub path_policy: PathPolicy,
}

#[derive(Deserialize)]
struct Args {
    path: String,
    edits: Vec<Edit>,
}

#[derive(Deserialize)]
struct Edit {
    old_text: String,
    new_text: String,
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &'static str {
        "file_edit"
    }
    fn description(&self) -> &'static str {
        "Apply search-and-replace edits to a file. Each old_text must appear exactly once. Returns a unified diff."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative path to the file" },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": { "type": "string", "description": "Exact text to find (must appear once)" },
                            "new_text": { "type": "string", "description": "Text to replace it with" }
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }
    fn require_approval(&self, _args: &Value) -> bool {
        self.approve
            || _args
                .get("path")
                .and_then(Value::as_str)
                .map(|p| self.path_policy.require_prompt_for_path(p))
                .unwrap_or(false)
    }
    async fn preview(&self, args: &Value) -> Option<ToolPreview> {
        let args: Args = serde_json::from_value(args.clone()).ok()?;
        let resolved = self.path_policy.resolve(&args.path);
        let original = tokio::fs::read_to_string(&resolved.normalized).await.ok()?;
        let mut working = original.clone();
        for edit in &args.edits {
            if edit.old_text.is_empty() || working.matches(&edit.old_text).count() != 1 {
                return Some(ToolPreview {
                    summary: format!("Edit {}", resolved.normalized.display()),
                    diff: None,
                    risk: Some(
                        "preview unavailable until each old_text matches exactly once".into(),
                    ),
                });
            }
            working = working.replacen(&edit.old_text, &edit.new_text, 1);
        }
        let mut risk = None;
        if resolved.outside_workspace {
            risk = Some(format!(
                "outside workspace root {}",
                self.path_policy.root().display()
            ));
        }
        Some(ToolPreview {
            summary: format!(
                "Edit {} ({} edits)",
                resolved.normalized.display(),
                args.edits.len()
            ),
            diff: Some(unified_diff(
                &original,
                &working,
                &resolved.normalized.display().to_string(),
            )),
            risk,
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if let Some(error) = self.path_policy.deny_path(&args.path) {
            return json!({ "error": error });
        }
        let resolved = self.path_policy.resolve(&args.path);
        let path = resolved.normalized.display().to_string();
        let original = match tokio::fs::read_to_string(&resolved.normalized).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return json!({ "error": format!("File not found: {path}") });
            }
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let mut working = original.clone();
        for (idx, edit) in args.edits.iter().enumerate() {
            if edit.old_text.is_empty() {
                return json!({ "error": format!("Edit {}: old_text is empty", idx + 1) });
            }
            let occurrences = working.matches(&edit.old_text).count();
            if occurrences == 0 {
                return json!({ "error": format!("Edit {}: old_text not found", idx + 1) });
            }
            if occurrences > 1 {
                return json!({
                    "error": format!(
                        "Edit {}: old_text appears {} times — make it unique",
                        idx + 1,
                        occurrences
                    )
                });
            }
            working = working.replacen(&edit.old_text, &edit.new_text, 1);
        }
        if let Err(e) = tokio::fs::write(&resolved.normalized, working.as_bytes()).await {
            return json!({ "error": e.to_string() });
        }
        json!({
            "edited": true,
            "path": path,
            "diff": unified_diff(&original, &working, &path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_identical_has_no_hunks() {
        let d = unified_diff("a\nb\nc", "a\nb\nc", "f.txt");
        assert_eq!(d, "--- f.txt\n+++ f.txt");
    }

    #[test]
    fn diff_single_replacement() {
        let d = unified_diff("a\nb\nc", "a\nB\nc", "f.txt");
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
        assert!(d.contains("@@ -2 +2 @@"));
    }

    #[test]
    fn diff_includes_added_line() {
        let d = unified_diff("a\nc", "a\nb\nc", "f.txt");
        assert!(d.contains("+b"));
        assert!(d.contains("@@ -2 +2 @@"));
    }

    #[test]
    fn diff_includes_removed_line() {
        let d = unified_diff("a\nb\nc", "a\nc", "f.txt");
        assert!(d.contains("-b"));
        assert!(d.contains("@@ -2 +2 @@"));
    }

    #[test]
    fn diff_header_uses_path() {
        let d = unified_diff("x", "y", "/abs/path/foo.rs");
        assert!(d.starts_with("--- /abs/path/foo.rs\n+++ /abs/path/foo.rs"));
    }

    #[tokio::test]
    async fn applies_unique_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "alpha\nbeta\ngamma").await.unwrap();

        let result = FileEditTool {
            approve: false,
            path_policy: PathPolicy::default(),
        }
        .execute(json!({
            "path": path.to_str().unwrap(),
            "edits": [{ "old_text": "beta", "new_text": "BETA" }]
        }))
        .await;

        assert!(result["edited"].as_bool().unwrap());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "alpha\nBETA\ngamma");
    }

    #[tokio::test]
    async fn rejects_non_unique_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "x\nx\nx").await.unwrap();

        let result = FileEditTool {
            approve: false,
            path_policy: PathPolicy::default(),
        }
        .execute(json!({
            "path": path.to_str().unwrap(),
            "edits": [{ "old_text": "x", "new_text": "y" }]
        }))
        .await;
        assert!(result["error"].as_str().unwrap().contains("3 times"));
    }

    #[tokio::test]
    async fn rejects_missing_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "alpha").await.unwrap();

        let result = FileEditTool {
            approve: false,
            path_policy: PathPolicy::default(),
        }
        .execute(json!({
            "path": path.to_str().unwrap(),
            "edits": [{ "old_text": "missing", "new_text": "x" }]
        }))
        .await;
        assert!(result["error"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn applies_sequential_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "one two three").await.unwrap();

        let result = FileEditTool {
            approve: false,
            path_policy: PathPolicy::default(),
        }
        .execute(json!({
            "path": path.to_str().unwrap(),
            "edits": [
                { "old_text": "one", "new_text": "ONE" },
                { "old_text": "three", "new_text": "THREE" }
            ]
        }))
        .await;

        assert!(result["edited"].as_bool().unwrap());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "ONE two THREE");
    }
}

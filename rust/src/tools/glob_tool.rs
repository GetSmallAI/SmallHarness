use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::Tool;

pub struct GlobTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

const IGNORE_DIRS: &[&str] = &["node_modules", ".git", "dist"];

fn is_ignored(rel: &std::path::Path) -> bool {
    rel.components().any(|c| {
        if let std::path::Component::Normal(name) = c {
            if let Some(s) = name.to_str() {
                return IGNORE_DIRS.contains(&s);
            }
        }
        false
    })
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "Find files by glob pattern. Skips node_modules/.git/dist. Returns up to 1000 paths."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. \"src/**/*.ts\"" },
                "path": { "type": "string", "description": "Directory to search in (default: cwd)" }
            },
            "required": ["pattern"]
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        let glob = match globset::GlobBuilder::new(&args.pattern)
            .literal_separator(true)
            .build()
        {
            Ok(g) => g.compile_matcher(),
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let root: PathBuf = args
            .path
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let pattern = args.pattern.clone();
        let root_for_walk = root.clone();
        let results: Vec<String> = tokio::task::spawn_blocking(move || {
            let mut out: Vec<String> = Vec::new();
            let walker = walkdir::WalkDir::new(&root_for_walk)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    if let Some(name) = e.file_name().to_str() {
                        !IGNORE_DIRS.contains(&name)
                    } else {
                        true
                    }
                });
            for entry in walker.flatten() {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&root_for_walk)
                    .unwrap_or(entry.path());
                if is_ignored(rel) {
                    continue;
                }
                if glob.is_match(rel) {
                    out.push(rel.to_string_lossy().into_owned());
                }
                if out.len() >= 2000 {
                    break;
                }
            }
            let _ = pattern;
            out
        })
        .await
        .unwrap_or_default();

        let count = results.len();
        let truncated = count > 1000;
        let matches: Vec<String> = results.into_iter().take(1000).collect();
        json!({ "matches": matches, "count": count, "truncated": truncated })
    }
}

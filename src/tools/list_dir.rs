use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::Tool;

pub struct ListDirTool;

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn description(&self) -> &'static str {
        "List directory contents (alphabetical). Up to 500 entries."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path (default: cwd)" }
            }
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        let path = args.path.unwrap_or_else(|| ".".into());
        let mut rd = match tokio::fs::read_dir(&path).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return json!({ "error": format!("Directory not found: {path}") });
            }
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let mut entries: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push(if is_dir { format!("{name}/") } else { name });
        }
        entries.sort();
        let total = entries.len();
        let truncated = total > 500;
        let entries: Vec<String> = entries.into_iter().take(500).collect();
        let count = entries.len();
        json!({
            "entries": entries,
            "count": count,
            "truncated": truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_entries_alphabetical_with_dir_suffix() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("b.txt"), "")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("a.txt"), "")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("subdir"))
            .await
            .unwrap();

        let result = ListDirTool
            .execute(json!({ "path": dir.path().to_str().unwrap() }))
            .await;

        let entries: Vec<&str> = result["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert_eq!(entries, vec!["a.txt", "b.txt", "subdir/"]);
        assert_eq!(result["count"].as_u64().unwrap(), 3);
        assert_eq!(result["truncated"].as_bool().unwrap(), false);
    }

    #[tokio::test]
    async fn missing_dir_returns_error() {
        let result = ListDirTool
            .execute(json!({ "path": "/totally/missing/dir/abc-xyz" }))
            .await;
        assert!(result["error"].as_str().unwrap().contains("not found"));
    }
}

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

use super::Tool;

pub struct FileWriteTool {
    pub approve: bool,
}

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &'static str {
        "file_write"
    }
    fn description(&self) -> &'static str {
        "Write content to a file. Creates parent directories if needed. Overwrites if the file exists."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative path to the file" },
                "content": { "type": "string", "description": "Full content to write" }
            },
            "required": ["path", "content"]
        })
    }
    fn require_approval(&self, _args: &Value) -> bool {
        self.approve
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if let Some(parent) = Path::new(&args.path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return json!({ "error": e.to_string() });
                }
            }
        }
        match tokio::fs::write(&args.path, args.content.as_bytes()).await {
            Ok(_) => json!({
                "written": true,
                "path": args.path,
                "bytes": args.content.len(),
            }),
            Err(e) => json!({ "error": e.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_file_to_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let result = FileWriteTool { approve: false }
            .execute(json!({
                "path": path.to_str().unwrap(),
                "content": "hello"
            }))
            .await;
        assert!(result["written"].as_bool().unwrap());
        assert_eq!(result["bytes"].as_u64().unwrap(), 5);
        let read = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(read, "hello");
    }

    #[tokio::test]
    async fn writes_file_creating_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/file.txt");
        let result = FileWriteTool { approve: false }
            .execute(json!({
                "path": path.to_str().unwrap(),
                "content": "deep"
            }))
            .await;
        assert!(result["written"].as_bool().unwrap());
        let read = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(read, "deep");
    }

    #[tokio::test]
    async fn write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o.txt");
        tokio::fs::write(&path, "old contents").await.unwrap();
        let _ = FileWriteTool { approve: false }
            .execute(json!({
                "path": path.to_str().unwrap(),
                "content": "new"
            }))
            .await;
        let read = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(read, "new");
    }

    #[test]
    fn approval_field_drives_require_approval() {
        let t1 = FileWriteTool { approve: true };
        let t2 = FileWriteTool { approve: false };
        let v = json!({});
        assert!(t1.require_approval(&v));
        assert!(!t2.require_approval(&v));
    }
}

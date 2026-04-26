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

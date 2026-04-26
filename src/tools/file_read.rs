use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::Tool;

pub struct FileReadTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

fn image_ext(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    for ext in ["jpeg", "jpg", "png", "gif", "webp"] {
        if lower.ends_with(&format!(".{ext}")) {
            return Some(match ext {
                "jpg" | "jpeg" => "jpeg",
                other => other,
            });
        }
    }
    None
}

fn b64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let b0 = rem[0] as u32;
        let b1 = if rem.len() > 1 { rem[1] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if rem.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        out.push('=');
    }
    out
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &'static str {
        "file_read"
    }
    fn description(&self) -> &'static str {
        "Read the contents of a file at the given path. Returns text or, for image files, base64 with mime type."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative path to the file" },
                "offset": { "type": "integer", "minimum": 1, "description": "Start reading from this line (1-indexed)" },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to return" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if let Some(ext) = image_ext(&args.path) {
            return match tokio::fs::read(&args.path).await {
                Ok(bytes) => json!({
                    "type": "image",
                    "mimeType": format!("image/{ext}"),
                    "data": b64_encode(&bytes),
                }),
                Err(e) => map_err(&args.path, e),
            };
        }
        let content = match tokio::fs::read_to_string(&args.path).await {
            Ok(c) => c,
            Err(e) => return map_err(&args.path, e),
        };
        let lines: Vec<&str> = content.split('\n').collect();
        let total = lines.len();
        let start = args
            .offset
            .map(|o| o.saturating_sub(1))
            .unwrap_or(0)
            .min(total);
        let end = args.limit.map(|l| start + l).unwrap_or(total).min(total);
        let slice = &lines[start..end];
        let mut obj = serde_json::Map::new();
        obj.insert("content".into(), json!(slice.join("\n")));
        obj.insert("totalLines".into(), json!(total));
        if end < total {
            obj.insert("truncated".into(), json!(true));
            obj.insert("nextOffset".into(), json!(end + 1));
        }
        Value::Object(obj)
    }
}

fn map_err(path: &str, e: std::io::Error) -> Value {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => json!({ "error": format!("File not found: {path}") }),
        PermissionDenied => json!({ "error": format!("Permission denied: {path}") }),
        _ => json!({ "error": e.to_string() }),
    }
}

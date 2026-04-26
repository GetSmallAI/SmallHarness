use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{PathPolicy, Tool};

pub struct GrepTool {
    pub path_policy: PathPolicy,
}

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default, rename = "ignoreCase")]
    ignore_case: Option<bool>,
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Search file contents by regex. Uses ripgrep when available. Returns up to 100 matches."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory or file to search (default: cwd)" },
                "glob": { "type": "string", "description": "File filter, e.g. \"*.ts\"" },
                "ignoreCase": { "type": "boolean" }
            },
            "required": ["pattern"]
        })
    }
    fn require_approval(&self, args: &Value) -> bool {
        args.get("path")
            .and_then(Value::as_str)
            .map(|p| self.path_policy.require_prompt_for_path(p))
            .unwrap_or(false)
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        let mut cmd = tokio::process::Command::new("rg");
        cmd.args([
            "--line-number",
            "--no-heading",
            "--max-count=100",
            "--color=never",
        ]);
        if args.ignore_case.unwrap_or(false) {
            cmd.arg("--ignore-case");
        }
        if let Some(g) = &args.glob {
            cmd.args(["--glob", g]);
        }
        cmd.arg(&args.pattern);
        let requested = args.path.unwrap_or_else(|| ".".into());
        if let Some(error) = self.path_policy.deny_path(&requested) {
            return json!({ "error": error });
        }
        let resolved = self.path_policy.resolve(&requested);
        cmd.arg(resolved.normalized);
        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return json!({ "error": "ripgrep (rg) not found. Install with `brew install ripgrep`." });
            }
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let code = output.status.code().unwrap_or(-1);
        if code == 1 {
            return json!({ "matches": [], "count": 0 });
        }
        if code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return json!({ "error": stderr.trim() });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let matches: Vec<Value> = stdout
            .split('\n')
            .filter(|l| !l.is_empty())
            .take(100)
            .map(|line| {
                let parts: Vec<&str> = line.splitn(3, ':').collect();
                if parts.len() == 3 {
                    if let Ok(n) = parts[1].parse::<u32>() {
                        return json!({
                            "file": parts[0],
                            "line": n,
                            "content": parts[2],
                        });
                    }
                }
                json!({ "content": line })
            })
            .collect();
        let count = matches.len();
        json!({ "matches": matches, "count": count })
    }
}

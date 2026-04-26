use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;

use super::PathPolicy;
use super::Tool;
use crate::cancel::CancellationToken;
use crate::config::ApprovalPolicy;

pub struct ShellTool {
    pub policy: ApprovalPolicy,
    pub path_policy: PathPolicy,
}

#[derive(Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
}

fn dangerous_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\brm\b|\bsudo\b|\bchmod\b|\bchown\b|\bdd\b|\bmkfs\b|>\s*/dev|--force\b|-rf?\b")
            .expect("dangerous regex")
    })
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn description(&self) -> &'static str {
        "Execute a shell command and return combined stdout/stderr. Output is truncated at 256KB."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "timeout": { "type": "integer", "minimum": 1, "description": "Timeout in seconds (default: 120)" }
            },
            "required": ["command"]
        })
    }
    fn require_approval(&self, args: &Value) -> bool {
        match self.policy {
            ApprovalPolicy::Always => true,
            ApprovalPolicy::Never => false,
            ApprovalPolicy::DangerousOnly => {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                dangerous_re().is_match(cmd) || self.path_policy.require_prompt_for_cwd()
            }
        }
    }
    async fn execute(&self, args: Value) -> Value {
        self.execute_cancelable(args, None).await
    }
    async fn execute_cancelable(&self, args: Value, cancel: Option<CancellationToken>) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if let Some(error) = self.path_policy.deny_cwd() {
            return json!({ "error": error });
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let timeout = Duration::from_secs(args.timeout.unwrap_or(120));

        let mut child = match tokio::process::Command::new(&shell)
            .args(["-c", &args.command])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return json!({
                    "output": e.to_string(),
                    "exitCode": 1,
                });
            }
        };

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let read_so = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut s = stdout;
            let _ = s.read_to_end(&mut buf).await;
            buf
        });
        let read_se = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut s = stderr;
            let _ = s.read_to_end(&mut buf).await;
            buf
        });

        let wait_fut = tokio::time::timeout(timeout, child.wait());
        let (exit_code, timed_out, cancelled) = if let Some(cancel) = cancel {
            tokio::select! {
                result = wait_fut => match result {
                    Ok(Ok(status)) => (status.code().unwrap_or(1), false, false),
                    Ok(Err(_)) => (1, false, false),
                    Err(_) => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        (-1, true, false)
                    }
                },
                _ = cancel.cancelled() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    (-1, false, true)
                }
            }
        } else {
            match wait_fut.await {
                Ok(Ok(status)) => (status.code().unwrap_or(1), false, false),
                Ok(Err(_)) => (1, false, false),
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    (-1, true, false)
                }
            }
        };

        let so = read_so.await.unwrap_or_default();
        let se = read_se.await.unwrap_or_default();

        const MAX_BYTES: usize = 256 * 1024;
        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&so));
        combined.push_str(&String::from_utf8_lossy(&se));
        if combined.len() > MAX_BYTES {
            let cutoff = combined
                .char_indices()
                .rev()
                .nth(0)
                .map(|_| combined.len() - MAX_BYTES)
                .unwrap_or(0);
            // Trim from the front, preserving the last MAX_BYTES.
            // Find a UTF-8 boundary at or after `cutoff`.
            let mut start = cutoff;
            while start < combined.len() && !combined.is_char_boundary(start) {
                start += 1;
            }
            combined = combined[start..].to_string();
        }
        let lines: Vec<&str> = combined.split('\n').collect();
        let truncated = lines.len() > 2000;
        let final_output = if truncated {
            lines[lines.len() - 2000..].join("\n")
        } else {
            combined.clone()
        };

        let mut obj = serde_json::Map::new();
        obj.insert(
            "output".into(),
            json!(if final_output.is_empty() {
                "(no output)".to_string()
            } else {
                final_output
            }),
        );
        obj.insert("exitCode".into(), json!(exit_code));
        if timed_out {
            obj.insert("timedOut".into(), json!(true));
        }
        if cancelled {
            obj.insert("cancelled".into(), json!(true));
        }
        if truncated {
            obj.insert("truncated".into(), json!(true));
        }
        Value::Object(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_flags_destructive_commands() {
        let re = dangerous_re();
        assert!(re.is_match("rm -rf /"));
        assert!(re.is_match("rm foo"));
        assert!(re.is_match("sudo apt install something"));
        assert!(re.is_match("chmod +x foo"));
        assert!(re.is_match("chown user:group file"));
        assert!(re.is_match("dd if=/dev/zero of=/dev/sda"));
        assert!(re.is_match("mkfs.ext4 /dev/sda1"));
        assert!(re.is_match("mv -rf src dest"));
        assert!(re.is_match("cargo install --force foo"));
    }

    #[test]
    fn dangerous_misses_safe_commands() {
        let re = dangerous_re();
        assert!(!re.is_match("ls -la"));
        assert!(!re.is_match("echo hello world"));
        assert!(!re.is_match("cat foo.txt"));
        assert!(!re.is_match("git status"));
        assert!(!re.is_match("cargo build --release"));
        assert!(!re.is_match("npm run dev"));
    }
}

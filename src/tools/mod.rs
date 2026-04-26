use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::config::{AgentConfig, ApprovalPolicy};

mod file_edit;
mod file_read;
mod file_write;
mod glob_tool;
mod grep;
mod list_dir;
mod shell;

pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob_tool::GlobTool;
pub use grep::GrepTool;
pub use list_dir::ListDirTool;
pub use shell::ShellTool;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    fn require_approval(&self, _args: &Value) -> bool {
        false
    }
    async fn execute(&self, args: Value) -> Value;
}

pub fn build_tools(config: &AgentConfig) -> Vec<Arc<dyn Tool>> {
    let approve_writes = config.approval_policy != ApprovalPolicy::Never;
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    for name in &config.tools {
        let t: Option<Arc<dyn Tool>> = match name.as_str() {
            "file_read" => Some(Arc::new(FileReadTool)),
            "file_write" => Some(Arc::new(FileWriteTool {
                approve: approve_writes,
            })),
            "file_edit" => Some(Arc::new(FileEditTool {
                approve: approve_writes,
            })),
            "glob" => Some(Arc::new(GlobTool)),
            "grep" => Some(Arc::new(GrepTool)),
            "list_dir" => Some(Arc::new(ListDirTool)),
            "shell" => Some(Arc::new(ShellTool {
                policy: config.approval_policy,
            })),
            _ => None,
        };
        if let Some(t) = t {
            out.push(t);
        }
    }
    out
}

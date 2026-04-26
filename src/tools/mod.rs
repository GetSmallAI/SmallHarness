use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::cancel::CancellationToken;
use crate::config::{AgentConfig, ApprovalPolicy};

mod apply_patch_tool;
mod diff;
mod file_edit;
mod file_read;
mod file_write;
mod glob_tool;
mod grep;
mod list_dir;
mod path_policy;
mod shell;

pub use apply_patch_tool::ApplyPatchTool;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob_tool::GlobTool;
pub use grep::GrepTool;
pub use list_dir::ListDirTool;
pub use path_policy::PathPolicy;
pub use shell::ShellTool;

#[derive(Debug, Clone)]
pub struct ToolPreview {
    pub summary: String,
    pub diff: Option<String>,
    pub risk: Option<String>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    fn require_approval(&self, _args: &Value) -> bool {
        false
    }
    async fn preview(&self, _args: &Value) -> Option<ToolPreview> {
        None
    }
    async fn execute(&self, args: Value) -> Value;
    async fn execute_cancelable(&self, args: Value, _cancel: Option<CancellationToken>) -> Value {
        self.execute(args).await
    }
}

pub fn build_tools(config: &AgentConfig) -> Vec<Arc<dyn Tool>> {
    let approve_writes = config.approval_policy != ApprovalPolicy::Never;
    let path_policy = PathPolicy::new(&config.workspace_root, config.outside_workspace);
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    for name in &config.tools {
        let t: Option<Arc<dyn Tool>> = match name.as_str() {
            "apply_patch" => Some(Arc::new(ApplyPatchTool {
                approve: approve_writes,
                path_policy: path_policy.clone(),
            })),
            "file_read" => Some(Arc::new(FileReadTool {
                path_policy: path_policy.clone(),
            })),
            "file_write" => Some(Arc::new(FileWriteTool {
                approve: approve_writes,
                path_policy: path_policy.clone(),
            })),
            "file_edit" => Some(Arc::new(FileEditTool {
                approve: approve_writes,
                path_policy: path_policy.clone(),
            })),
            "glob" => Some(Arc::new(GlobTool {
                path_policy: path_policy.clone(),
            })),
            "grep" => Some(Arc::new(GrepTool {
                path_policy: path_policy.clone(),
            })),
            "list_dir" => Some(Arc::new(ListDirTool {
                path_policy: path_policy.clone(),
            })),
            "shell" => Some(Arc::new(ShellTool {
                policy: config.approval_policy,
                path_policy: path_policy.clone(),
            })),
            _ => None,
        };
        if let Some(t) = t {
            out.push(t);
        }
    }
    out
}

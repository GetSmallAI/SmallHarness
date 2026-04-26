use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::cancel::CancellationToken;
use crate::config::{AgentConfig, ApprovalPolicy, ToolSelection};

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

fn enabled(config: &AgentConfig, name: &str) -> bool {
    config.tools.iter().any(|t| t == name)
}

fn push_if_enabled(out: &mut Vec<String>, config: &AgentConfig, name: &str) {
    if enabled(config, name) && !out.iter().any(|t| t == name) {
        out.push(name.to_string());
    }
}

pub fn select_tool_names(config: &AgentConfig, prompt: &str) -> Vec<String> {
    if config.tool_selection == ToolSelection::Fixed {
        return config.tools.clone();
    }

    let lower = prompt.to_lowercase();
    let fileish = [
        "file",
        "files",
        "repo",
        "repository",
        "code",
        "src",
        "read",
        "open",
        "search",
        "grep",
        "find",
        "list",
        "directory",
        "folder",
        "where is",
        "inspect",
        "review",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let editish = [
        "edit",
        "change",
        "modify",
        "update",
        "fix",
        "implement",
        "add support",
        "refactor",
        "write",
        "create",
        "delete",
        "patch",
        "build it",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let shellish = [
        "run ", "execute", "command", "terminal", "shell", "test", "cargo ", "npm ", "git ",
        "build", "lint", "check",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    let mut out = Vec::new();
    if fileish || editish {
        push_if_enabled(&mut out, config, "file_read");
        push_if_enabled(&mut out, config, "grep");
        push_if_enabled(&mut out, config, "list_dir");
        push_if_enabled(&mut out, config, "glob");
    }
    if editish {
        push_if_enabled(&mut out, config, "file_edit");
        push_if_enabled(&mut out, config, "apply_patch");
        push_if_enabled(&mut out, config, "file_write");
    }
    if shellish {
        push_if_enabled(&mut out, config, "shell");
    }
    out
}

pub fn build_tools_for_names(config: &AgentConfig, names: &[String]) -> Vec<Arc<dyn Tool>> {
    let approve_writes = config.approval_policy != ApprovalPolicy::Never;
    let path_policy = PathPolicy::new(&config.workspace_root, config.outside_workspace);
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    for name in names {
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

pub fn build_tools(config: &AgentConfig) -> Vec<Arc<dyn Tool>> {
    build_tools_for_names(config, &config.tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ToolSelection;

    #[test]
    fn auto_chat_uses_no_tools() {
        let config = AgentConfig {
            tool_selection: ToolSelection::Auto,
            ..Default::default()
        };
        assert!(select_tool_names(&config, "hello there").is_empty());
    }

    #[test]
    fn auto_file_question_uses_read_tools() {
        let config = AgentConfig {
            tool_selection: ToolSelection::Auto,
            ..Default::default()
        };
        let names = select_tool_names(&config, "search the repo for config");
        assert!(names.contains(&"file_read".to_string()));
        assert!(names.contains(&"grep".to_string()));
        assert!(names.contains(&"list_dir".to_string()));
    }

    #[test]
    fn fixed_mode_uses_enabled_pool() {
        let config = AgentConfig {
            tool_selection: ToolSelection::Fixed,
            ..Default::default()
        };
        assert_eq!(select_tool_names(&config, "hello"), config.tools);
    }
}

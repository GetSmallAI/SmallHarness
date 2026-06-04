use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::cancel::CancellationToken;
use crate::config::{AgentConfig, ApprovalPolicy, ToolSelection};

mod apply_patch_tool;
mod batch_edit;
pub mod diff;
mod file_edit;
mod file_read;
mod file_write;
mod glob_tool;
mod grep;
mod list_dir;
mod path_policy;
mod repo_search;
mod run_tests;
mod shell;
mod ship_status;
mod subagent;
mod update_plan;
mod web_fetch;

pub use apply_patch_tool::{patch_changed_files, ApplyPatchTool};
pub use batch_edit::BatchEditTool;
pub use diff::unified_diff;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob_tool::GlobTool;
pub use grep::GrepTool;
pub use list_dir::ListDirTool;
pub use path_policy::PathPolicy;
pub use repo_search::RepoSearchTool;
pub use run_tests::RunTestsTool;
pub use shell::ShellTool;
pub use ship_status::ShipStatusTool;
pub use subagent::SubagentTool;
pub use update_plan::UpdatePlanTool;
pub use web_fetch::WebFetchTool;

/// Base64-encode raw bytes for use in a data URL (e.g. `data:image/png;base64,...`).
/// Re-exported from `file_read` so callers outside the tools module (like
/// `/image`) don't have to duplicate the implementation.
pub fn image_base64_for_data_url(bytes: &[u8]) -> String {
    file_read::b64_encode(bytes)
}

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
    let testish = ["failing", "verify", "unit test", "test suite"]
        .iter()
        .any(|needle| lower.contains(needle))
        || (lower.contains("test") && !lower.contains("latest"));
    let shipish = [
        "ship",
        "ready to commit",
        "ready to ship",
        "shipcheck",
        "handoff",
        "release",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let batchish = [
        "multi-file",
        "multiple files",
        "across files",
        "batch edit",
        "coordinated",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    let mut out = Vec::new();
    if fileish || editish {
        if config.project_memory.enabled {
            push_if_enabled(&mut out, config, "repo_search");
        }
        push_if_enabled(&mut out, config, "file_read");
        push_if_enabled(&mut out, config, "grep");
        push_if_enabled(&mut out, config, "list_dir");
        push_if_enabled(&mut out, config, "glob");
        // Delegated read-only investigation keeps deep exploration out of the
        // parent context.
        push_if_enabled(&mut out, config, "task");
    }
    if editish {
        push_if_enabled(&mut out, config, "file_edit");
        push_if_enabled(&mut out, config, "apply_patch");
        push_if_enabled(&mut out, config, "file_write");
    }
    // Multi-step work (editing or running things) benefits from a visible plan.
    if editish || shellish {
        push_if_enabled(&mut out, config, "update_plan");
    }
    if shellish {
        push_if_enabled(&mut out, config, "shell");
    }
    if testish || shellish {
        push_if_enabled(&mut out, config, "run_tests");
    }
    if batchish && editish {
        push_if_enabled(&mut out, config, "batch_edit");
    }
    if shipish {
        push_if_enabled(&mut out, config, "ship_status");
    }
    out
}

/// Returns true when a tool result indicates the workspace was mutated (for ship-loop hooks).
pub fn tool_output_mutated_workspace(tool_name: &str, output: &str) -> bool {
    let Ok(output_json) = serde_json::from_str::<Value>(output) else {
        return false;
    };
    if output_json.get("error").is_some() {
        return false;
    }
    match tool_name {
        "file_write" => output_json
            .get("written")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "file_edit" => output_json
            .get("edited")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "apply_patch" | "batch_edit" => output_json
            .get("applied")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

pub fn mutation_tool_names() -> &'static [&'static str] {
    &["file_write", "file_edit", "apply_patch", "batch_edit"]
}

pub fn is_mutation_tool(name: &str) -> bool {
    mutation_tool_names().contains(&name)
}

/// Built-in tools that only read state and have no side effects on the
/// workspace, so the agent loop can safely run several of them concurrently
/// within a single step. `shell` and `run_tests` are excluded (they can mutate
/// or run arbitrary commands), as are MCP tools (unknown side effects).
pub fn read_only_tool_names() -> &'static [&'static str] {
    &[
        "file_read",
        "grep",
        "list_dir",
        "glob",
        "repo_search",
        "ship_status",
    ]
}

pub fn is_read_only_tool(name: &str) -> bool {
    read_only_tool_names().contains(&name)
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
            "repo_search" => Some(Arc::new(RepoSearchTool {
                config: config.clone(),
            })),
            "shell" => Some(Arc::new(ShellTool {
                policy: config.approval_policy,
                path_policy: path_policy.clone(),
            })),
            "run_tests" => Some(Arc::new(RunTestsTool {
                workspace_root: config.workspace_root.clone(),
                policy: config.approval_policy,
            })),
            "batch_edit" => Some(Arc::new(BatchEditTool {
                workspace_root: config.workspace_root.clone(),
            })),
            "ship_status" => Some(Arc::new(ShipStatusTool {
                workspace_root: config.workspace_root.clone(),
            })),
            "update_plan" => Some(Arc::new(UpdatePlanTool)),
            "task" => {
                // The subagent resolves its own backend/model from config so it
                // can run an independent loop. Its toolset is curated read-only
                // (see SubagentTool), so this never recurses into another `task`.
                let backend = crate::backends::backend(config.backend);
                let model =
                    crate::backends::default_model(&backend, config.model_override.as_deref());
                Some(Arc::new(SubagentTool {
                    http: reqwest::Client::new(),
                    backend,
                    model,
                    config: config.clone(),
                }))
            }
            "web_fetch" => Some(Arc::new(WebFetchTool {
                http: reqwest::Client::new(),
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
    fn repo_search_is_opt_in_not_default() {
        // repo_search is no longer in the default pool; auto-selection can't
        // surface it unless the user enables it.
        let config = AgentConfig {
            tool_selection: ToolSelection::Auto,
            ..Default::default()
        };
        assert!(!config.tools.contains(&"repo_search".to_string()));
        let names = select_tool_names(&config, "search the repo for config");
        assert!(!names.contains(&"repo_search".to_string()));
    }

    #[test]
    fn auto_repo_search_requires_memory_enabled() {
        // When repo_search IS enabled, auto-selection still gates it on memory.
        let base_tools = vec![
            "repo_search".to_string(),
            "file_read".to_string(),
            "grep".to_string(),
            "list_dir".to_string(),
        ];
        let enabled = AgentConfig {
            tools: base_tools.clone(),
            ..Default::default()
        };
        assert!(select_tool_names(&enabled, "search the repo for config")
            .contains(&"repo_search".to_string()));

        let disabled = AgentConfig {
            tools: base_tools,
            project_memory: crate::config::ProjectMemoryConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!select_tool_names(&disabled, "search the repo for config")
            .contains(&"repo_search".to_string()));
    }

    #[test]
    fn auto_verify_prompt_includes_run_tests() {
        let config = AgentConfig {
            tools: vec!["run_tests".into(), "file_read".into(), "file_edit".into()],
            ..Default::default()
        };
        let names = select_tool_names(&config, "verify the failing unit test");
        assert!(names.contains(&"run_tests".to_string()));
    }

    #[test]
    fn auto_ship_prompt_includes_ship_status() {
        let config = AgentConfig {
            tools: vec!["ship_status".into(), "file_read".into()],
            ..Default::default()
        };
        let names = select_tool_names(&config, "is this ready to ship?");
        assert!(names.contains(&"ship_status".to_string()));
    }

    #[test]
    fn batch_edit_dry_run_does_not_count_as_mutation() {
        let output = r#"{"preview":{},"applied":false,"dryRun":true,"successful":[],"failed":[]}"#;
        assert!(!tool_output_mutated_workspace("batch_edit", output));
    }

    #[test]
    fn batch_edit_validation_failure_does_not_count_as_mutation() {
        let output = r#"{"preview":{},"applied":false,"successful":[],"failed":[{"filePath":"a.rs","error":"not found"}]}"#;
        assert!(!tool_output_mutated_workspace("batch_edit", output));
    }

    #[test]
    fn batch_edit_apply_counts_as_mutation() {
        let output = r#"{"preview":{},"applied":true,"successful":["a.rs"],"failed":[]}"#;
        assert!(tool_output_mutated_workspace("batch_edit", output));
    }

    #[test]
    fn read_only_classifier_excludes_side_effecting_tools() {
        assert!(is_read_only_tool("file_read"));
        assert!(is_read_only_tool("grep"));
        assert!(is_read_only_tool("repo_search"));
        // Side-effecting or arbitrary-command tools are never parallelized.
        assert!(!is_read_only_tool("shell"));
        assert!(!is_read_only_tool("run_tests"));
        assert!(!is_read_only_tool("file_edit"));
        // MCP and unknown tools are not in the safe set.
        assert!(!is_read_only_tool("mcp__fs__read_file"));
        assert!(!is_read_only_tool("web_fetch"));
    }

    #[test]
    fn mutation_and_read_only_sets_are_disjoint() {
        for m in mutation_tool_names() {
            assert!(!is_read_only_tool(m), "{m} must not be read-only");
        }
    }

    #[test]
    fn file_edit_success_counts_as_mutation() {
        let output = r#"{"edited":true,"path":"src/main.rs","diff":"..."}"#;
        assert!(tool_output_mutated_workspace("file_edit", output));
    }

    #[test]
    fn mutation_detection_uses_structured_fields_not_error_substrings() {
        let output = r#"{"edited":true,"path":"src/main.rs","diff":"+ let label = \"error\";"}"#;
        assert!(tool_output_mutated_workspace("file_edit", output));

        let output = r#"{"path":"src/main.rs","diff":"..."}"#;
        assert!(!tool_output_mutated_workspace("file_edit", output));
    }
}

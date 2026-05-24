use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::agent::{run_agent, AgentEvent, ApprovalProvider, RunResult};
use crate::backends::{validate, BackendDescriptor};
use crate::config::AgentConfig;
use crate::openai::{build_http_client, ChatMessage};
use crate::project_memory::render_system_prompt_with_memory;
use crate::session::{init_session_dir, new_session_path, save_message};
use crate::test_integration::run_tests;
use crate::tools::{build_tools_for_names, select_tool_names, ToolPreview};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEvalFixture {
    pub id: String,
    pub prompt: String,
    pub workspace: Option<String>,
    pub checks: Vec<AgentEvalCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvalCheck {
    TestsPass,
    FileContains { path: String, needle: String },
    GitClean,
    ToolUsed { name: String },
    AssistantMentions { needle: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEvalCheckResult {
    pub check: AgentEvalCheck,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEvalRunResult {
    pub fixture_id: String,
    pub model: String,
    pub backend: String,
    pub passed: bool,
    pub checks: Vec<AgentEvalCheckResult>,
    pub elapsed_ms: u128,
    pub steps: usize,
    pub tool_calls: Vec<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub transcript_path: String,
    pub error: Option<String>,
}

struct EvalApproval;

#[async_trait]
impl ApprovalProvider for EvalApproval {
    async fn approve(
        &mut self,
        _name: &str,
        _args: &Value,
        _preview: Option<&ToolPreview>,
    ) -> bool {
        true
    }
}

pub fn builtin_fixtures() -> Vec<AgentEvalFixture> {
    vec![
        AgentEvalFixture {
            id: "read-and-explain".into(),
            prompt: "Read src/lib.rs and briefly explain what the `add` function does.".into(),
            workspace: Some("fix-failing-test".into()),
            checks: vec![
                AgentEvalCheck::ToolUsed {
                    name: "file_read".into(),
                },
                AgentEvalCheck::AssistantMentions {
                    needle: "add".into(),
                },
            ],
        },
        AgentEvalFixture {
            id: "fix-failing-test".into(),
            prompt: "The tests are failing. Fix the bug so `cargo test` passes.".into(),
            workspace: Some("fix-failing-test".into()),
            checks: vec![AgentEvalCheck::TestsPass],
        },
        AgentEvalFixture {
            id: "small-refactor".into(),
            prompt:
                "Rename the function `greet_user` to `welcome_user` across all files in this crate."
                    .into(),
            workspace: Some("small-refactor".into()),
            checks: vec![
                AgentEvalCheck::FileContains {
                    path: "src/main.rs".into(),
                    needle: "welcome_user".into(),
                },
                AgentEvalCheck::TestsPass,
            ],
        },
        AgentEvalFixture {
            id: "add-feature".into(),
            prompt: "Add a `mul` function and a passing test for it.".into(),
            workspace: Some("add-feature".into()),
            checks: vec![
                AgentEvalCheck::FileContains {
                    path: "src/lib.rs".into(),
                    needle: "fn mul".into(),
                },
                AgentEvalCheck::TestsPass,
            ],
        },
    ]
}

pub fn fixture_by_id(id: &str) -> Option<AgentEvalFixture> {
    builtin_fixtures()
        .into_iter()
        .find(|fixture| fixture.id == id)
}

pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("evals/fixtures")
}

pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn prepare_fixture_workspace(
    fixture: &AgentEvalFixture,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().to_path_buf();
    if let Some(rel) = &fixture.workspace {
        let src = fixtures_root().join(rel);
        if !src.exists() {
            anyhow::bail!("fixture workspace not found: {}", src.display());
        }
        copy_dir_all(&src, &workspace)?;
    }
    Ok((temp, workspace))
}

pub fn prepare_playground_workspace(
    session_dir: &str,
    fixture_id: &str,
    fixture: &AgentEvalFixture,
) -> Result<PathBuf> {
    let stamp = Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ");
    let dest = Path::new(session_dir)
        .join("play")
        .join(format!("{fixture_id}-{stamp}"));
    fs::create_dir_all(&dest)?;
    if let Some(rel) = &fixture.workspace {
        let src = fixtures_root().join(rel);
        if !src.exists() {
            anyhow::bail!("fixture workspace not found: {}", src.display());
        }
        copy_dir_all(&src, &dest)?;
    }
    Ok(dest)
}

pub fn init_git_if_needed(workspace: &Path) -> Result<()> {
    if workspace.join(".git").exists() {
        return Ok(());
    }
    let status = std::process::Command::new("git")
        .args(["init"])
        .current_dir(workspace)
        .status()?;
    if !status.success() {
        anyhow::bail!("git init failed");
    }
    let _ = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(workspace)
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "fixture baseline", "--allow-empty"])
        .current_dir(workspace)
        .status();
    Ok(())
}

pub fn count_assistant_steps(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
        .count()
}

pub fn evaluate_checks(
    workspace: &Path,
    checks: &[AgentEvalCheck],
    run: &RunResult,
    tool_calls: &[String],
) -> Vec<AgentEvalCheckResult> {
    checks
        .iter()
        .map(|check| {
            let (passed, detail) = match check {
                AgentEvalCheck::TestsPass => match run_tests(workspace.to_str().unwrap(), None) {
                    Ok(result) => (
                        result.failed == 0 && result.exit_code == 0,
                        format!(
                            "total={} passed={} failed={} exit={}",
                            result.total, result.passed, result.failed, result.exit_code
                        ),
                    ),
                    Err(e) => (false, e.to_string()),
                },
                AgentEvalCheck::FileContains { path, needle } => {
                    let full = workspace.join(path);
                    match fs::read_to_string(&full) {
                        Ok(content) => (
                            content.contains(needle),
                            format!("{} contains '{needle}'", full.display()),
                        ),
                        Err(e) => (false, e.to_string()),
                    }
                }
                AgentEvalCheck::GitClean => match std::process::Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(workspace)
                    .output()
                {
                    Ok(out) if out.status.success() => {
                        let clean = out.stdout.is_empty();
                        (
                            clean,
                            if clean {
                                "working tree clean".into()
                            } else {
                                format!(
                                    "dirty files: {}",
                                    String::from_utf8_lossy(&out.stdout).trim()
                                )
                            },
                        )
                    }
                    Ok(out) => (false, String::from_utf8_lossy(&out.stderr).into()),
                    Err(e) => (false, e.to_string()),
                },
                AgentEvalCheck::ToolUsed { name } => {
                    let used = tool_calls.iter().any(|tool| tool == name);
                    (
                        used,
                        if used {
                            format!("tool {name} was called")
                        } else {
                            format!("tool {name} was not called")
                        },
                    )
                }
                AgentEvalCheck::AssistantMentions { needle } => {
                    let text: String = run
                        .messages
                        .iter()
                        .filter_map(|m| match m {
                            ChatMessage::Assistant { content, .. } => content.as_deref(),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let found = text.to_lowercase().contains(&needle.to_lowercase());
                    (
                        found,
                        if found {
                            format!("assistant mentioned '{needle}'")
                        } else {
                            "assistant did not mention needle".into()
                        },
                    )
                }
            };
            AgentEvalCheckResult {
                check: check.clone(),
                passed,
                detail,
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_eval(
    config: &AgentConfig,
    backend_desc: &BackendDescriptor,
    model: &str,
    fixture: &AgentEvalFixture,
) -> Result<AgentEvalRunResult> {
    validate(backend_desc)?;
    let (_temp, workspace) = prepare_fixture_workspace(fixture)?;
    init_git_if_needed(&workspace)?;
    let workspace_root = workspace.to_str().unwrap().to_string();

    let mut eval_config = config.clone();
    eval_config.workspace_root = workspace_root.clone();
    eval_config.approval_policy = crate::config::ApprovalPolicy::Never;
    eval_config.tool_selection = crate::config::ToolSelection::Fixed;
    eval_config.apply_operator_mode(crate::config::OperatorMode::Ship);

    let active_tool_names = select_tool_names(&eval_config, &fixture.prompt);
    let system_prompt = render_system_prompt_with_memory(
        &eval_config,
        backend_desc,
        &active_tool_names,
        &fixture.prompt,
    );
    let messages = vec![
        ChatMessage::System {
            content: system_prompt,
        },
        ChatMessage::User {
            content: fixture.prompt.clone().into(),
        },
    ];
    let tools = build_tools_for_names(&eval_config, &active_tool_names);
    let http = build_http_client();
    let mut tool_calls = Vec::new();
    let start = Instant::now();
    let mut approval = EvalApproval;
    let run = run_agent(
        &http,
        backend_desc,
        model,
        messages,
        tools,
        eval_config.max_steps,
        |event| {
            if let AgentEvent::ToolCall { name, .. } = event {
                tool_calls.push(name);
            }
        },
        Some(&mut approval as &mut dyn ApprovalProvider),
        None,
        None,
        None,
    )
    .await;
    let elapsed_ms = start.elapsed().as_millis();

    init_session_dir(&eval_config.session_dir)?;
    let transcript_path = new_session_path(&eval_config.session_dir);
    if let Ok(result) = &run {
        for message in &result.messages {
            let _ = save_message(&transcript_path, message);
        }
    }

    match run {
        Ok(result) => {
            let checks = evaluate_checks(&workspace, &fixture.checks, &result, &tool_calls);
            let passed = checks.iter().all(|c| c.passed);
            Ok(AgentEvalRunResult {
                fixture_id: fixture.id.clone(),
                model: model.to_string(),
                backend: backend_desc.name.as_str().to_string(),
                passed,
                checks,
                elapsed_ms,
                steps: count_assistant_steps(&result.messages),
                tool_calls,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                transcript_path: transcript_path.display().to_string(),
                error: None,
            })
        }
        Err(e) => Ok(AgentEvalRunResult {
            fixture_id: fixture.id.clone(),
            model: model.to_string(),
            backend: backend_desc.name.as_str().to_string(),
            passed: false,
            checks: Vec::new(),
            elapsed_ms,
            steps: 0,
            tool_calls,
            input_tokens: 0,
            output_tokens: 0,
            transcript_path: transcript_path.display().to_string(),
            error: Some(e.to_string()),
        }),
    }
}

pub fn render_agent_eval_markdown(results: &[AgentEvalRunResult]) -> String {
    let mut md = String::from("# Small Harness Agent Eval\n\n");
    md.push_str("| model | fixture | passed | steps | latency ms | tools_used |\n");
    md.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for result in results {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            result.model,
            result.fixture_id,
            if result.passed { "yes" } else { "no" },
            result.steps,
            result.elapsed_ms,
            result.tool_calls.join(", ")
        ));
    }
    md.push('\n');
    for result in results {
        md.push_str(&format!("## {} · {}\n\n", result.model, result.fixture_id));
        if let Some(error) = &result.error {
            md.push_str(&format!("**Error:** {error}\n\n"));
        }
        for check in &result.checks {
            md.push_str(&format!(
                "- [{}] {:?}: {}\n",
                if check.passed { "x" } else { " " },
                check.check,
                check.detail
            ));
        }
        md.push('\n');
    }
    md
}

#[allow(dead_code)]
pub async fn run_agent_eval_suite(
    config: &AgentConfig,
    backend_desc: &BackendDescriptor,
    model: &str,
    fixture_ids: &[String],
) -> Result<Vec<AgentEvalRunResult>> {
    let mut out = Vec::new();
    for id in fixture_ids {
        let fixture = fixture_by_id(id).ok_or_else(|| anyhow!("unknown fixture: {id}"))?;
        out.push(run_agent_eval(config, backend_desc, model, &fixture).await?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_fixtures_include_required_ids() {
        let ids: Vec<_> = builtin_fixtures().into_iter().map(|f| f.id).collect();
        assert!(ids.contains(&"read-and-explain".to_string()));
        assert!(ids.contains(&"fix-failing-test".to_string()));
        assert!(ids.contains(&"small-refactor".to_string()));
        assert!(ids.contains(&"add-feature".to_string()));
    }

    #[test]
    fn prepare_playground_workspace_copies_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = fixture_by_id("fix-failing-test").unwrap();
        let dest = prepare_playground_workspace(
            dir.path().to_str().unwrap(),
            "fix-failing-test",
            &fixture,
        )
        .unwrap();
        assert!(dest.join("Cargo.toml").exists());
        assert!(dest.join("src/lib.rs").exists());
    }

    #[test]
    fn file_contains_check_detects_needle() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("note.txt"), "hello world").unwrap();
        let checks = evaluate_checks(
            dir.path(),
            &[AgentEvalCheck::FileContains {
                path: "note.txt".into(),
                needle: "world".into(),
            }],
            &RunResult {
                messages: vec![],
                input_tokens: 0,
                output_tokens: 0,
                transcript_rewritten: false,
                conversation_summary: None,
            },
            &[],
        );
        assert!(checks[0].passed);
    }
}

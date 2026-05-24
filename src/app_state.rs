use anyhow::Result;
use std::path::PathBuf;

use crate::agent_eval::AgentEvalCheckResult;
use crate::approval::ApprovalCache;
use crate::backends::{backend, default_model, validate, BackendDescriptor};
use crate::config::AgentConfig;
use crate::openai::ChatMessage;
use crate::renderer::TuiRenderer;
use crate::turn_checkpoint::CheckpointStack;

#[derive(Debug, Clone)]
pub struct PlayRestoreSnapshot {
    pub config: AgentConfig,
    pub checkpoints_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct PlaySession {
    pub fixture_id: String,
    pub sandbox_root: PathBuf,
    pub restore: PlayRestoreSnapshot,
}

#[derive(Debug, Clone)]
pub struct PlayScorecard {
    pub fixture_id: String,
    pub model: String,
    pub elapsed_ms: u128,
    pub steps: usize,
    pub tool_calls: Vec<String>,
    pub checks: Vec<AgentEvalCheckResult>,
    pub passed: bool,
}

#[derive(Debug, Clone)]
pub struct FixRestoreSnapshot {
    pub config: AgentConfig,
    pub checkpoints_enabled: bool,
}

pub struct AppState {
    pub config: AgentConfig,
    pub http: reqwest::Client,
    pub backend: BackendDescriptor,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub session_dir: String,
    pub session_path: PathBuf,
    pub total_in: u32,
    pub total_out: u32,
    pub context_guard_notice: Option<String>,
    pub conversation_summary: Option<String>,
    pub checkpoint_stack: CheckpointStack,
    pub checkpoints_enabled: bool,
    pub play_session: Option<PlaySession>,
    pub last_play_scorecard: Option<PlayScorecard>,
    pub approval_cache: ApprovalCache,
    pub renderer: TuiRenderer,
    pub warmed_fingerprint: Option<u64>,
    pub tests_ran_this_session: bool,
}

impl AppState {
    pub fn rebuild_client(&mut self) -> Result<()> {
        let new_backend = backend(self.config.backend);
        validate(&new_backend)?;
        self.backend = new_backend;
        Ok(())
    }

    pub fn resolve_model(&mut self) {
        self.model = default_model(
            &self.backend,
            &self.config.profile,
            self.config.model_override.as_deref(),
            &self.config.profiles,
        );
    }

    pub fn reset_session(&mut self) {
        self.session_path = crate::session::new_session_path(&self.session_dir);
    }

    pub fn in_play_session(&self) -> bool {
        self.play_session.is_some()
    }
}

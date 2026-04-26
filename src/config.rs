use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::backends::{BackendName, ProfileName};

pub const ALL_TOOL_NAMES: &[&str] = &[
    "file_read",
    "file_write",
    "file_edit",
    "glob",
    "grep",
    "list_dir",
    "shell",
];

pub fn is_tool_name(s: &str) -> bool {
    ALL_TOOL_NAMES.contains(&s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    Always,
    Never,
    DangerousOnly,
}

impl ApprovalPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalPolicy::Always => "always",
            ApprovalPolicy::Never => "never",
            ApprovalPolicy::DangerousOnly => "dangerous-only",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            "dangerous-only" => Some(Self::DangerousOnly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolDisplay {
    Emoji,
    Grouped,
    Minimal,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputStyle {
    Block,
    Bordered,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoaderStyle {
    Gradient,
    Spinner,
    Minimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayConfig {
    #[serde(rename = "toolDisplay", default = "default_tool_display")]
    pub tool_display: ToolDisplay,
    #[serde(rename = "inputStyle", default = "default_input_style")]
    pub input_style: InputStyle,
    #[serde(rename = "loaderText", default = "default_loader_text")]
    pub loader_text: String,
    #[serde(rename = "loaderStyle", default = "default_loader_style")]
    pub loader_style: LoaderStyle,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(rename = "showBanner", default = "default_true")]
    pub show_banner: bool,
}

fn default_true() -> bool {
    true
}
fn default_tool_display() -> ToolDisplay {
    ToolDisplay::Grouped
}
fn default_input_style() -> InputStyle {
    InputStyle::Bordered
}
fn default_loader_text() -> String {
    "Thinking".into()
}
fn default_loader_style() -> LoaderStyle {
    LoaderStyle::Spinner
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            tool_display: default_tool_display(),
            input_style: default_input_style(),
            loader_text: default_loader_text(),
            loader_style: default_loader_style(),
            reasoning: false,
            show_banner: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub backend: BackendName,
    pub profile: ProfileName,
    pub model_override: Option<String>,
    pub system_prompt: String,
    pub max_steps: usize,
    pub session_dir: String,
    pub approval_policy: ApprovalPolicy,
    pub tools: Vec<String>,
    pub display: DisplayConfig,
    pub slash_commands: bool,
}

const SYSTEM_PROMPT: &str = concat!(
    "You are a coding assistant running on the user's local machine via a small open-weight LLM.\n",
    "\n",
    "Always respond in English unless the user writes to you in another language.\n",
    "\n",
    "Available tools: {tools}.\n",
    "\n",
    "When to use tools vs answer directly:\n",
    "- For greetings, casual chat, or questions you can answer from general knowledge, respond in plain text. Do NOT call a tool.\n",
    "- Only call a tool when the user's request actually needs filesystem access.\n",
    "- When you do call a tool, emit a real tool call — not a JSON description in your text response.\n",
    "\n",
    "When working with code:\n",
    "- Make minimal targeted edits consistent with existing style.\n",
    "- Be concise. The user can read the diff.\n",
    "\n",
    "Current working directory: {cwd}",
);

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            backend: BackendName::Ollama,
            profile: ProfileName::MacMini16gb,
            model_override: None,
            system_prompt: SYSTEM_PROMPT.into(),
            max_steps: 20,
            session_dir: ".sessions".into(),
            approval_policy: ApprovalPolicy::Always,
            tools: vec![
                "file_read".into(),
                "file_edit".into(),
                "grep".into(),
                "list_dir".into(),
            ],
            display: DisplayConfig::default(),
            slash_commands: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    backend: Option<String>,
    profile: Option<String>,
    #[serde(rename = "modelOverride")]
    model_override: Option<String>,
    #[serde(rename = "systemPrompt")]
    system_prompt: Option<String>,
    #[serde(rename = "maxSteps")]
    max_steps: Option<usize>,
    #[serde(rename = "sessionDir")]
    session_dir: Option<String>,
    #[serde(rename = "approvalPolicy")]
    approval_policy: Option<String>,
    tools: Option<Vec<String>>,
    display: Option<DisplayConfig>,
    #[serde(rename = "slashCommands")]
    slash_commands: Option<bool>,
}

impl AgentConfig {
    pub fn render_system_prompt(&self) -> String {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.system_prompt
            .replace("{cwd}", &cwd)
            .replace("{tools}", &self.tools.join(", "))
    }
}

pub fn load_config() -> AgentConfig {
    let mut config = AgentConfig::default();

    let path = Path::new("agent.config.json");
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(file) = serde_json::from_str::<FileConfig>(&text) {
                if let Some(b) = file.backend.as_deref().and_then(BackendName::parse) {
                    config.backend = b;
                }
                if let Some(p) = file.profile.as_deref().and_then(ProfileName::parse) {
                    config.profile = p;
                }
                if let Some(m) = file.model_override {
                    config.model_override = Some(m);
                }
                if let Some(sp) = file.system_prompt {
                    config.system_prompt = sp;
                }
                if let Some(s) = file.max_steps {
                    config.max_steps = s;
                }
                if let Some(d) = file.session_dir {
                    config.session_dir = d;
                }
                if let Some(a) = file.approval_policy.as_deref().and_then(ApprovalPolicy::parse) {
                    config.approval_policy = a;
                }
                if let Some(t) = file.tools {
                    let valid: Vec<String> =
                        t.into_iter().filter(|n| is_tool_name(n)).collect();
                    if !valid.is_empty() {
                        config.tools = valid;
                    }
                }
                if let Some(d) = file.display {
                    config.display = d;
                }
                if let Some(sc) = file.slash_commands {
                    config.slash_commands = sc;
                }
            }
        }
    }

    if let Ok(s) = std::env::var("BACKEND") {
        if let Some(b) = BackendName::parse(&s) {
            config.backend = b;
        }
    }
    if let Ok(s) = std::env::var("PROFILE") {
        if let Some(p) = ProfileName::parse(&s) {
            config.profile = p;
        }
    }
    if let Ok(m) = std::env::var("AGENT_MODEL") {
        config.model_override = Some(m);
    }
    if let Ok(s) = std::env::var("AGENT_MAX_STEPS") {
        if let Ok(n) = s.parse::<usize>() {
            config.max_steps = n;
        }
    }
    if let Ok(s) = std::env::var("APPROVAL_POLICY") {
        if let Some(p) = ApprovalPolicy::parse(&s) {
            config.approval_policy = p;
        }
    }
    if let Ok(s) = std::env::var("AGENT_TOOLS") {
        let requested: Vec<String> = s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && is_tool_name(s))
            .collect();
        if !requested.is_empty() {
            config.tools = requested;
        }
    }

    config
}

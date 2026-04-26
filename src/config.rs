use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::backends::BackendName;

pub const ALL_TOOL_NAMES: &[&str] = &[
    "apply_patch",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolSelection {
    Auto,
    Fixed,
}

impl ToolSelection {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolSelection::Auto => "auto",
            ToolSelection::Fixed => "fixed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "fixed" => Some(Self::Fixed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutsideWorkspace {
    Prompt,
    Deny,
    Allow,
}

impl OutsideWorkspace {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutsideWorkspace::Prompt => "prompt",
            OutsideWorkspace::Deny => "deny",
            OutsideWorkspace::Allow => "allow",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "prompt" => Some(Self::Prompt),
            "deny" => Some(Self::Deny),
            "allow" => Some(Self::Allow),
            _ => None,
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    #[serde(rename = "maxMessages", default)]
    pub max_messages: Option<usize>,
    #[serde(rename = "maxBytes", default)]
    pub max_bytes: Option<usize>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_messages: Some(40),
            max_bytes: Some(256 * 1024),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(rename = "maxEntries", default = "default_history_max_entries")]
    pub max_entries: usize,
    #[serde(default)]
    pub path: Option<String>,
}

fn default_history_max_entries() -> usize {
    200
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: default_history_max_entries(),
            path: None,
        }
    }
}

pub type ProfileModels = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub backend: BackendName,
    pub profile: String,
    pub model_override: Option<String>,
    pub system_prompt: String,
    pub max_steps: usize,
    pub session_dir: String,
    pub workspace_root: String,
    pub outside_workspace: OutsideWorkspace,
    pub approval_policy: ApprovalPolicy,
    pub tools: Vec<String>,
    pub tool_selection: ToolSelection,
    pub display: DisplayConfig,
    pub slash_commands: bool,
    pub context: ContextConfig,
    pub history: HistoryConfig,
    pub profiles: BTreeMap<String, ProfileModels>,
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
            profile: "mac-mini-16gb".into(),
            model_override: None,
            system_prompt: SYSTEM_PROMPT.into(),
            max_steps: 20,
            session_dir: ".sessions".into(),
            workspace_root: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .display()
                .to_string(),
            outside_workspace: OutsideWorkspace::Prompt,
            approval_policy: ApprovalPolicy::Always,
            tools: vec![
                "file_read".into(),
                "file_edit".into(),
                "grep".into(),
                "list_dir".into(),
            ],
            tool_selection: ToolSelection::Auto,
            display: DisplayConfig::default(),
            slash_commands: true,
            context: ContextConfig::default(),
            history: HistoryConfig::default(),
            profiles: BTreeMap::new(),
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
    #[serde(rename = "workspaceRoot")]
    workspace_root: Option<String>,
    #[serde(rename = "outsideWorkspace")]
    outside_workspace: Option<String>,
    #[serde(rename = "approvalPolicy")]
    approval_policy: Option<String>,
    tools: Option<Vec<String>>,
    #[serde(rename = "toolSelection")]
    tool_selection: Option<String>,
    display: Option<DisplayConfig>,
    #[serde(rename = "slashCommands")]
    slash_commands: Option<bool>,
    context: Option<ContextConfig>,
    history: Option<HistoryConfig>,
    profiles: Option<BTreeMap<String, ProfileModels>>,
}

impl AgentConfig {
    pub fn render_system_prompt(&self) -> String {
        self.render_system_prompt_for_tools(&self.tools)
    }

    pub fn render_system_prompt_for_tools(&self, tools: &[String]) -> String {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let tool_list = if tools.is_empty() {
            "none".to_string()
        } else {
            tools.join(", ")
        };
        self.system_prompt
            .replace("{cwd}", &cwd)
            .replace("{tools}", &tool_list)
    }

    pub fn history_path(&self) -> String {
        self.history.path.clone().unwrap_or_else(|| {
            Path::new(&self.session_dir)
                .join("history.jsonl")
                .display()
                .to_string()
        })
    }
}

fn parse_dotenv_file(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let mut value = value.trim().to_string();
        if value.len() >= 2 {
            let quoted = (value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\''));
            if quoted {
                value = value[1..value.len() - 1].to_string();
            }
        }
        out.insert(key.to_string(), value);
    }
    out
}

fn dotenv_values() -> BTreeMap<String, String> {
    let mut out = parse_dotenv_file(Path::new(".env"));
    out.extend(parse_dotenv_file(Path::new(".env.local")));
    out
}

fn layered_env(dotenv: &BTreeMap<String, String>, key: &str) -> Option<String> {
    std::env::var(key).ok().or_else(|| dotenv.get(key).cloned())
}

pub fn load_config() -> AgentConfig {
    let mut config = AgentConfig::default();
    let dotenv = dotenv_values();

    let path = Path::new("agent.config.json");
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(file) = serde_json::from_str::<FileConfig>(&text) {
                if let Some(b) = file.backend.as_deref().and_then(BackendName::parse) {
                    config.backend = b;
                }
                if let Some(p) = file.profile {
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
                if let Some(w) = file.workspace_root {
                    config.workspace_root = w;
                }
                if let Some(o) = file
                    .outside_workspace
                    .as_deref()
                    .and_then(OutsideWorkspace::parse)
                {
                    config.outside_workspace = o;
                }
                if let Some(a) = file
                    .approval_policy
                    .as_deref()
                    .and_then(ApprovalPolicy::parse)
                {
                    config.approval_policy = a;
                }
                if let Some(t) = file.tools {
                    let valid: Vec<String> = t.into_iter().filter(|n| is_tool_name(n)).collect();
                    if !valid.is_empty() {
                        config.tools = valid;
                    }
                }
                if let Some(s) = file
                    .tool_selection
                    .as_deref()
                    .and_then(ToolSelection::parse)
                {
                    config.tool_selection = s;
                }
                if let Some(d) = file.display {
                    config.display = d;
                }
                if let Some(sc) = file.slash_commands {
                    config.slash_commands = sc;
                }
                if let Some(c) = file.context {
                    config.context = c;
                }
                if let Some(h) = file.history {
                    config.history = h;
                }
                if let Some(p) = file.profiles {
                    config.profiles = p;
                }
            }
        }
    }

    if let Some(s) = layered_env(&dotenv, "BACKEND") {
        if let Some(b) = BackendName::parse(&s) {
            config.backend = b;
        }
    }
    if let Some(s) = layered_env(&dotenv, "PROFILE") {
        config.profile = s;
    }
    if let Some(m) = layered_env(&dotenv, "AGENT_MODEL") {
        config.model_override = Some(m);
    }
    if let Some(s) = layered_env(&dotenv, "AGENT_MAX_STEPS") {
        if let Ok(n) = s.parse::<usize>() {
            config.max_steps = n;
        }
    }
    if let Some(s) = layered_env(&dotenv, "APPROVAL_POLICY") {
        if let Some(p) = ApprovalPolicy::parse(&s) {
            config.approval_policy = p;
        }
    }
    if let Some(s) = layered_env(&dotenv, "AGENT_TOOLS") {
        let requested: Vec<String> = s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && is_tool_name(s))
            .collect();
        if !requested.is_empty() {
            config.tools = requested;
        }
    }
    if let Some(s) = layered_env(&dotenv, "AGENT_TOOL_SELECTION") {
        if let Some(selection) = ToolSelection::parse(&s) {
            config.tool_selection = selection;
        }
    }
    if let Some(s) = layered_env(&dotenv, "WORKSPACE_ROOT") {
        config.workspace_root = s;
    }
    if let Some(s) = layered_env(&dotenv, "OUTSIDE_WORKSPACE") {
        if let Some(p) = OutsideWorkspace::parse(&s) {
            config.outside_workspace = p;
        }
    }
    if let Some(s) = layered_env(&dotenv, "AGENT_CONTEXT_MAX_MESSAGES") {
        if let Ok(n) = s.parse::<usize>() {
            config.context.max_messages = Some(n);
        }
    }
    if let Some(s) = layered_env(&dotenv, "AGENT_CONTEXT_MAX_BYTES") {
        if let Ok(n) = s.parse::<usize>() {
            config.context.max_bytes = Some(n);
        }
    }
    if let Some(s) = layered_env(&dotenv, "AGENT_HISTORY") {
        config.history.enabled = s != "false" && s != "0";
    }
    if let Some(s) = layered_env(&dotenv, "AGENT_HISTORY_MAX_ENTRIES") {
        if let Ok(n) = s.parse::<usize>() {
            config.history.max_entries = n.max(1);
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dotenv_quotes_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(
            &path,
            "A=one\n# nope\nB=\"two words\"\nC='three words'\nBAD\n",
        )
        .unwrap();
        let parsed = parse_dotenv_file(&path);
        assert_eq!(parsed.get("A").map(String::as_str), Some("one"));
        assert_eq!(parsed.get("B").map(String::as_str), Some("two words"));
        assert_eq!(parsed.get("C").map(String::as_str), Some("three words"));
        assert!(!parsed.contains_key("BAD"));
    }

    #[test]
    fn process_env_wins_over_dotenv_values() {
        let mut dotenv = BTreeMap::new();
        dotenv.insert("SMALL_HARNESS_TEST_LAYER".into(), "dotenv".into());
        std::env::set_var("SMALL_HARNESS_TEST_LAYER", "process");
        assert_eq!(
            layered_env(&dotenv, "SMALL_HARNESS_TEST_LAYER").as_deref(),
            Some("process")
        );
        std::env::remove_var("SMALL_HARNESS_TEST_LAYER");
    }
}

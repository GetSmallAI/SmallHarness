use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendName {
    Ollama,
    LmStudio,
    Mlx,
    Openrouter,
}

impl BackendName {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendName::Ollama => "ollama",
            BackendName::LmStudio => "lm-studio",
            BackendName::Mlx => "mlx",
            BackendName::Openrouter => "openrouter",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ollama" => Some(Self::Ollama),
            "lm-studio" => Some(Self::LmStudio),
            "mlx" => Some(Self::Mlx),
            "openrouter" => Some(Self::Openrouter),
            _ => None,
        }
    }
    pub fn all() -> &'static [BackendName] {
        &[Self::Ollama, Self::LmStudio, Self::Mlx, Self::Openrouter]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileName {
    MacMini16gb,
    MacStudio32gb,
}

impl ProfileName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProfileName::MacMini16gb => "mac-mini-16gb",
            ProfileName::MacStudio32gb => "mac-studio-32gb",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mac-mini-16gb" => Some(Self::MacMini16gb),
            "mac-studio-32gb" => Some(Self::MacStudio32gb),
            _ => None,
        }
    }
    pub fn all() -> &'static [ProfileName] {
        &[Self::MacMini16gb, Self::MacStudio32gb]
    }
}

#[derive(Debug, Clone)]
pub struct BackendDescriptor {
    pub name: BackendName,
    pub base_url: String,
    pub api_key: String,
    #[allow(dead_code)]
    pub is_local: bool,
}

pub fn backend(name: BackendName) -> BackendDescriptor {
    match name {
        BackendName::Ollama => BackendDescriptor {
            name,
            base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".into()),
            api_key: "ollama".into(),
            is_local: true,
        },
        BackendName::LmStudio => BackendDescriptor {
            name,
            base_url: std::env::var("LM_STUDIO_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:1234/v1".into()),
            api_key: "lm-studio".into(),
            is_local: true,
        },
        BackendName::Mlx => BackendDescriptor {
            name,
            base_url: std::env::var("MLX_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:8080/v1".into()),
            api_key: "mlx".into(),
            is_local: true,
        },
        BackendName::Openrouter => BackendDescriptor {
            name,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: std::env::var("OPENROUTER_API_KEY").unwrap_or_default(),
            is_local: false,
        },
    }
}

pub fn default_model(b: &BackendDescriptor, p: ProfileName, override_: Option<&str>) -> String {
    if let Some(m) = override_ {
        return m.to_string();
    }
    match (b.name, p) {
        (BackendName::Ollama, ProfileName::MacMini16gb) => "qwen2.5-coder:7b",
        (BackendName::Ollama, ProfileName::MacStudio32gb) => "qwen2.5-coder:14b",
        (BackendName::LmStudio, ProfileName::MacMini16gb) => "qwen2.5-coder-7b-instruct",
        (BackendName::LmStudio, ProfileName::MacStudio32gb) => "qwen2.5-coder-14b-instruct",
        (BackendName::Mlx, ProfileName::MacMini16gb) => {
            "mlx-community/Qwen2.5-Coder-7B-Instruct-4bit"
        }
        (BackendName::Mlx, ProfileName::MacStudio32gb) => {
            "mlx-community/Qwen2.5-Coder-14B-Instruct-4bit"
        }
        (BackendName::Openrouter, _) => "qwen/qwen-2.5-coder-32b-instruct",
    }
    .to_string()
}

pub fn validate(b: &BackendDescriptor) -> Result<()> {
    if matches!(b.name, BackendName::Openrouter) && b.api_key.is_empty() {
        return Err(anyhow!(
            "OPENROUTER_API_KEY is required when BACKEND=openrouter."
        ));
    }
    Ok(())
}

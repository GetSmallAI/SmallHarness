use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendName {
    Ollama,
    LmStudio,
    Mlx,
    LlamaCpp,
    Openrouter,
}

impl BackendName {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendName::Ollama => "ollama",
            BackendName::LmStudio => "lm-studio",
            BackendName::Mlx => "mlx",
            BackendName::LlamaCpp => "llamacpp",
            BackendName::Openrouter => "openrouter",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ollama" => Some(Self::Ollama),
            "lm-studio" => Some(Self::LmStudio),
            "mlx" => Some(Self::Mlx),
            "llamacpp" | "llama-cpp" | "llama.cpp" => Some(Self::LlamaCpp),
            "openrouter" => Some(Self::Openrouter),
            _ => None,
        }
    }
    pub fn all() -> &'static [BackendName] {
        &[
            Self::Ollama,
            Self::LmStudio,
            Self::Mlx,
            Self::LlamaCpp,
            Self::Openrouter,
        ]
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
        BackendName::LlamaCpp => BackendDescriptor {
            name,
            base_url: std::env::var("LLAMACPP_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:8080/v1".into()),
            api_key: std::env::var("LLAMACPP_API_KEY")
                .unwrap_or_else(|_| "sk-no-key-required".into()),
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

pub fn default_model(
    b: &BackendDescriptor,
    profile: &str,
    override_: Option<&str>,
    custom_profiles: &BTreeMap<String, BTreeMap<String, String>>,
) -> String {
    if let Some(m) = override_ {
        return m.to_string();
    }
    if let Some(models) = custom_profiles.get(profile) {
        if let Some(model) = models.get(b.name.as_str()) {
            return model.clone();
        }
    }
    match (b.name, profile) {
        (BackendName::Ollama, "mac-mini-16gb") => "qwen2.5-coder:7b",
        (BackendName::Ollama, "mac-studio-32gb") => "qwen2.5-coder:14b",
        (BackendName::LmStudio, "mac-mini-16gb") => "qwen2.5-coder-7b-instruct",
        (BackendName::LmStudio, "mac-studio-32gb") => "qwen2.5-coder-14b-instruct",
        (BackendName::Mlx, "mac-mini-16gb") => "mlx-community/Qwen2.5-Coder-7B-Instruct-4bit",
        (BackendName::Mlx, "mac-studio-32gb") => "mlx-community/Qwen2.5-Coder-14B-Instruct-4bit",
        (BackendName::LlamaCpp, _) => "gpt-3.5-turbo",
        (BackendName::Openrouter, _) => "qwen/qwen-2.5-coder-32b-instruct",
        (BackendName::Ollama, _) => "qwen2.5-coder:7b",
        (BackendName::LmStudio, _) => "qwen2.5-coder-7b-instruct",
        (BackendName::Mlx, _) => "mlx-community/Qwen2.5-Coder-7B-Instruct-4bit",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: BackendName) -> BackendDescriptor {
        BackendDescriptor {
            name,
            base_url: String::new(),
            api_key: String::new(),
            is_local: true,
        }
    }

    #[test]
    fn parses_llamacpp_aliases() {
        assert_eq!(BackendName::parse("llamacpp"), Some(BackendName::LlamaCpp));
        assert_eq!(BackendName::parse("llama-cpp"), Some(BackendName::LlamaCpp));
        assert_eq!(BackendName::parse("llama.cpp"), Some(BackendName::LlamaCpp));
    }

    #[test]
    fn lists_llamacpp_as_switchable_backend() {
        assert!(BackendName::all().contains(&BackendName::LlamaCpp));
    }

    #[test]
    fn defaults_llamacpp_to_openai_example_model() {
        let profiles = BTreeMap::new();
        let model = default_model(
            &descriptor(BackendName::LlamaCpp),
            "mac-mini-16gb",
            None,
            &profiles,
        );
        assert_eq!(model, "gpt-3.5-turbo");
    }
}

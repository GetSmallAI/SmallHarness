use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendName {
    Ollama,
    LmStudio,
    Mlx,
    LlamaCpp,
    Openrouter,
    OpenAi,
    OpenAiCodex,
}

impl BackendName {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendName::Ollama => "ollama",
            BackendName::LmStudio => "lm-studio",
            BackendName::Mlx => "mlx",
            BackendName::LlamaCpp => "llamacpp",
            BackendName::Openrouter => "openrouter",
            BackendName::OpenAi => "openai",
            BackendName::OpenAiCodex => "openai-codex",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ollama" => Some(Self::Ollama),
            "lm-studio" => Some(Self::LmStudio),
            "mlx" => Some(Self::Mlx),
            "llamacpp" | "llama-cpp" | "llama.cpp" => Some(Self::LlamaCpp),
            "openrouter" => Some(Self::Openrouter),
            "openai" | "open-ai" => Some(Self::OpenAi),
            "openai-codex" | "open-ai-codex" | "codex" | "chatgpt" => Some(Self::OpenAiCodex),
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
            Self::OpenAi,
            Self::OpenAiCodex,
        ]
    }
    /// True for backends that talk to a process on the user's machine, false
    /// for hosted APIs. Used to gate cloud-aware behavior (handoff refusal,
    /// recommend filtering, capability scoring) so adding a new hosted
    /// backend doesn't require touching every cloud check.
    pub fn is_local(&self) -> bool {
        match self {
            Self::Ollama | Self::LmStudio | Self::Mlx | Self::LlamaCpp => true,
            Self::Openrouter | Self::OpenAi | Self::OpenAiCodex => false,
        }
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
        BackendName::OpenAi => BackendDescriptor {
            name,
            base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            is_local: false,
        },
        BackendName::OpenAiCodex => BackendDescriptor {
            name,
            base_url: std::env::var("OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api".into()),
            // OAuth access tokens are loaded/refreshed lazily from auth.json by
            // the Codex Responses adapter.  Keep this empty so callers don't
            // accidentally treat ChatGPT subscription auth as an API key.
            api_key: String::new(),
            is_local: false,
        },
    }
}

/// The default model for a backend when the user hasn't set an override
/// (`modelOverride` / `AGENT_MODEL` / `/model`). One sensible default per
/// backend — local backends default to a 7B coder that runs on modest
/// hardware; bump it with `/model` if you have the headroom.
pub fn default_model(b: &BackendDescriptor, override_: Option<&str>) -> String {
    if let Some(m) = override_ {
        if matches!(b.name, BackendName::OpenAiCodex) {
            return crate::codex_responses::canonical_codex_model(m)
                .unwrap_or("gpt-5.5")
                .to_string();
        }
        return m.to_string();
    }
    match b.name {
        BackendName::Ollama => "qwen2.5-coder:7b",
        BackendName::LmStudio => "qwen2.5-coder-7b-instruct",
        BackendName::Mlx => "mlx-community/Qwen2.5-Coder-7B-Instruct-4bit",
        BackendName::LlamaCpp => "gpt-3.5-turbo",
        BackendName::Openrouter => "qwen/qwen-2.5-coder-32b-instruct",
        BackendName::OpenAi => "gpt-4o-mini",
        BackendName::OpenAiCodex => "gpt-5.5",
    }
    .to_string()
}

pub fn validate(b: &BackendDescriptor) -> Result<()> {
    if matches!(b.name, BackendName::Openrouter) && b.api_key.is_empty() {
        return Err(anyhow!(
            "OPENROUTER_API_KEY is required when BACKEND=openrouter."
        ));
    }
    if matches!(b.name, BackendName::OpenAi) && b.api_key.is_empty() {
        return Err(anyhow!("OPENAI_API_KEY is required when BACKEND=openai."));
    }
    if matches!(b.name, BackendName::OpenAiCodex)
        && crate::auth::AuthStore::load()
            .get_oauth("openai-codex")
            .is_none()
    {
        return Err(anyhow!(
            "ChatGPT/Codex login is required when BACKEND=openai-codex. Run `/login openai-codex`."
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
        let model = default_model(&descriptor(BackendName::LlamaCpp), None);
        assert_eq!(model, "gpt-3.5-turbo");
    }

    #[test]
    fn parses_openai_aliases() {
        assert_eq!(BackendName::parse("openai"), Some(BackendName::OpenAi));
        assert_eq!(BackendName::parse("open-ai"), Some(BackendName::OpenAi));
        assert_eq!(BackendName::OpenAi.as_str(), "openai");
    }

    #[test]
    fn lists_openai_as_switchable_backend() {
        assert!(BackendName::all().contains(&BackendName::OpenAi));
    }

    #[test]
    fn is_local_marks_hosted_backends_as_cloud() {
        assert!(BackendName::Ollama.is_local());
        assert!(BackendName::LmStudio.is_local());
        assert!(BackendName::Mlx.is_local());
        assert!(BackendName::LlamaCpp.is_local());
        assert!(!BackendName::Openrouter.is_local());
        assert!(!BackendName::OpenAi.is_local());
        assert!(!BackendName::OpenAiCodex.is_local());
    }

    #[test]
    fn defaults_openai_to_gpt_4o_mini() {
        let model = default_model(&descriptor(BackendName::OpenAi), None);
        assert_eq!(model, "gpt-4o-mini");
    }

    #[test]
    fn parses_openai_codex_aliases() {
        assert_eq!(
            BackendName::parse("openai-codex"),
            Some(BackendName::OpenAiCodex)
        );
        assert_eq!(BackendName::parse("codex"), Some(BackendName::OpenAiCodex));
        assert_eq!(
            BackendName::parse("chatgpt"),
            Some(BackendName::OpenAiCodex)
        );
        assert_eq!(BackendName::OpenAiCodex.as_str(), "openai-codex");
    }

    #[test]
    fn lists_openai_codex_as_switchable_backend() {
        assert!(BackendName::all().contains(&BackendName::OpenAiCodex));
    }

    #[test]
    fn defaults_openai_codex_to_codex_model() {
        let model = default_model(&descriptor(BackendName::OpenAiCodex), None);
        assert_eq!(model, "gpt-5.5");
    }

    #[test]
    fn ignores_non_codex_override_for_openai_codex() {
        let model = default_model(&descriptor(BackendName::OpenAiCodex), Some("gpt-5-codex"));
        assert_eq!(model, "gpt-5.5");
    }

    #[test]
    fn normalizes_openai_codex_model_aliases() {
        let model = default_model(&descriptor(BackendName::OpenAiCodex), Some("5.5"));
        assert_eq!(model, "gpt-5.5");
    }

    #[test]
    fn validate_requires_openai_api_key() {
        let mut desc = descriptor(BackendName::OpenAi);
        desc.is_local = false;
        assert!(validate(&desc).is_err());
        desc.api_key = "sk-test".into();
        assert!(validate(&desc).is_ok());
    }
}

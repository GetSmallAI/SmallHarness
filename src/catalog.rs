use crate::backends::BackendName;

/// Per-model context window and per-million-token pricing.
///
/// Prices are USD per 1M tokens. `0.0` means "free / local". The table is
/// best-effort and may drift as providers update pricing; treat the surfaced
/// numbers as a sanity check, not a contract.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelInfo {
    pub id: &'static str,
    pub context_tokens: u32,
    pub input_per_mtoken_usd: f32,
    pub output_per_mtoken_usd: f32,
    /// True when the model can accept image content parts. Drives whether
    /// `/image` is allowed to attach images for the next turn.
    pub vision: bool,
}

const OPENAI_MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "gpt-4o",
        context_tokens: 128_000,
        input_per_mtoken_usd: 2.50,
        output_per_mtoken_usd: 10.00,
        vision: true,
    },
    ModelInfo {
        id: "gpt-4o-mini",
        context_tokens: 128_000,
        input_per_mtoken_usd: 0.15,
        output_per_mtoken_usd: 0.60,
        vision: true,
    },
    ModelInfo {
        id: "gpt-4-turbo",
        context_tokens: 128_000,
        input_per_mtoken_usd: 10.00,
        output_per_mtoken_usd: 30.00,
        vision: true,
    },
    ModelInfo {
        id: "gpt-4",
        context_tokens: 8_192,
        input_per_mtoken_usd: 30.00,
        output_per_mtoken_usd: 60.00,
        vision: false,
    },
    ModelInfo {
        id: "gpt-3.5-turbo",
        context_tokens: 16_385,
        input_per_mtoken_usd: 0.50,
        output_per_mtoken_usd: 1.50,
        vision: false,
    },
    ModelInfo {
        id: "o1",
        context_tokens: 200_000,
        input_per_mtoken_usd: 15.00,
        output_per_mtoken_usd: 60.00,
        vision: true,
    },
    ModelInfo {
        id: "o1-mini",
        context_tokens: 128_000,
        input_per_mtoken_usd: 3.00,
        output_per_mtoken_usd: 12.00,
        vision: false,
    },
    ModelInfo {
        id: "o1-preview",
        context_tokens: 128_000,
        input_per_mtoken_usd: 15.00,
        output_per_mtoken_usd: 60.00,
        vision: false,
    },
    ModelInfo {
        id: "o3-mini",
        context_tokens: 200_000,
        input_per_mtoken_usd: 1.10,
        output_per_mtoken_usd: 4.40,
        vision: false,
    },
];

fn table_for(backend: BackendName) -> &'static [ModelInfo] {
    match backend {
        BackendName::OpenAi => OPENAI_MODELS,
        // Local backends don't have meaningful $-per-token; OpenRouter
        // pricing varies per model and is best looked up live.
        _ => &[],
    }
}

/// Look up catalog metadata for a model id.
///
/// Tries exact match first, then the longest known prefix — so versioned ids
/// like `gpt-4o-2024-11-20` resolve to the `gpt-4o` entry, while
/// `gpt-4o-mini-2024-07-18` correctly picks `gpt-4o-mini` (longer prefix wins).
pub fn lookup(backend: BackendName, model_id: &str) -> Option<&'static ModelInfo> {
    let table = table_for(backend);
    if let Some(exact) = table.iter().find(|m| m.id == model_id) {
        return Some(exact);
    }
    table
        .iter()
        .filter(|m| {
            model_id == m.id
                || model_id
                    .strip_prefix(m.id)
                    .map(|rest| rest.starts_with('-'))
                    .unwrap_or(false)
        })
        .max_by_key(|m| m.id.len())
}

/// Format a one-line cost label suitable for appending to a model row.
///
/// Returns `None` if the catalog has no entry (caller should render the bare
/// id). Cost is omitted entirely for entries where both rates are 0.
pub fn format_cost_label(info: &ModelInfo) -> String {
    let ctx = format_context(info.context_tokens);
    if info.input_per_mtoken_usd == 0.0 && info.output_per_mtoken_usd == 0.0 {
        format!("{ctx} ctx")
    } else {
        format!(
            "{ctx} ctx · ${:.2}/${:.2} per Mtoken",
            info.input_per_mtoken_usd, info.output_per_mtoken_usd
        )
    }
}

/// Cost in USD for a single turn given catalog rates. Returns None when the
/// model isn't in the catalog (caller decides whether to mark the session
/// total as a lower bound or just omit cost). Returns Some(0.0) for entries
/// whose rates are 0 (e.g. cataloged-but-free models).
pub fn turn_cost_usd(
    backend: BackendName,
    model_id: &str,
    tokens_in: u32,
    tokens_out: u32,
) -> Option<f64> {
    let info = lookup(backend, model_id)?;
    let in_cost = tokens_in as f64 * info.input_per_mtoken_usd as f64 / 1_000_000.0;
    let out_cost = tokens_out as f64 * info.output_per_mtoken_usd as f64 / 1_000_000.0;
    Some(in_cost + out_cost)
}

/// Format a USD amount for the status line. Sub-cent values use four
/// decimals so a $0.0003 turn doesn't display as "$0.00".
pub fn format_usd(amount: f64) -> String {
    if amount >= 0.01 {
        format!("${amount:.2}")
    } else if amount > 0.0 {
        format!("${amount:.4}")
    } else {
        "$0.00".into()
    }
}

fn format_context(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f32 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_returns_entry() {
        let info = lookup(BackendName::OpenAi, "gpt-4o-mini").unwrap();
        assert_eq!(info.id, "gpt-4o-mini");
        assert_eq!(info.context_tokens, 128_000);
    }

    #[test]
    fn versioned_id_resolves_to_base_entry() {
        let info = lookup(BackendName::OpenAi, "gpt-4o-2024-11-20").unwrap();
        assert_eq!(info.id, "gpt-4o");
    }

    #[test]
    fn longest_prefix_wins_for_mini_variants() {
        let info = lookup(BackendName::OpenAi, "gpt-4o-mini-2024-07-18").unwrap();
        assert_eq!(info.id, "gpt-4o-mini");
    }

    #[test]
    fn prefix_must_break_on_dash_not_substring() {
        // "gpt-4o" should not match an id that just happens to start with it
        // without a dash boundary (defensive — no such id exists today but the
        // matcher should be principled).
        assert!(lookup(BackendName::OpenAi, "gpt-4omega").is_none());
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(lookup(BackendName::OpenAi, "nonexistent-model").is_none());
    }

    #[test]
    fn local_backends_have_no_catalog() {
        assert!(lookup(BackendName::Ollama, "qwen2.5-coder:7b").is_none());
        assert!(lookup(BackendName::LmStudio, "qwen2.5-coder-7b-instruct").is_none());
    }

    #[test]
    fn openrouter_has_no_catalog() {
        assert!(lookup(BackendName::Openrouter, "qwen/qwen-2.5-coder-32b-instruct").is_none());
    }

    #[test]
    fn cost_label_renders_context_and_pricing() {
        let info = lookup(BackendName::OpenAi, "gpt-4o-mini").unwrap();
        let label = format_cost_label(info);
        assert!(label.contains("128k ctx"));
        assert!(label.contains("$0.15"));
        assert!(label.contains("$0.60"));
        assert!(label.contains("per Mtoken"));
    }

    #[test]
    fn context_formatting_uses_k_and_m_suffixes() {
        assert_eq!(format_context(8_192), "8k");
        assert_eq!(format_context(128_000), "128k");
        assert_eq!(format_context(1_500_000), "1.5m");
        assert_eq!(format_context(500), "500");
    }

    #[test]
    fn turn_cost_uses_catalog_rates() {
        // gpt-4o-mini: $0.15 in / $0.60 out per Mtoken
        // 1_000_000 in -> $0.15; 100_000 out -> $0.06; total $0.21
        let cost = turn_cost_usd(BackendName::OpenAi, "gpt-4o-mini", 1_000_000, 100_000).unwrap();
        assert!((cost - 0.21).abs() < 0.0001, "got {cost}");
    }

    #[test]
    fn turn_cost_is_none_for_uncataloged_model() {
        assert!(turn_cost_usd(BackendName::OpenAi, "future-model-9001", 1000, 100).is_none());
        assert!(turn_cost_usd(BackendName::Ollama, "qwen2.5-coder:7b", 1000, 100).is_none());
    }

    #[test]
    fn turn_cost_zero_for_zero_tokens() {
        let cost = turn_cost_usd(BackendName::OpenAi, "gpt-4o", 0, 0).unwrap();
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn format_usd_renders_sub_cent_with_extra_precision() {
        assert_eq!(format_usd(0.0), "$0.00");
        assert_eq!(format_usd(0.0003), "$0.0003");
        assert_eq!(format_usd(0.01), "$0.01");
        assert_eq!(format_usd(1.234), "$1.23");
        assert_eq!(format_usd(42.0), "$42.00");
    }
}

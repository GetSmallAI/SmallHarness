use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::backends::{BackendDescriptor, BackendName};
use crate::config::ToolSelection;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityStatus {
    pub ok: bool,
    pub detail: String,
}

impl CapabilityStatus {
    pub fn ok(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            detail: detail.into(),
        }
    }

    pub fn fail(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u128>,
    pub total_ms: u128,
    pub output_chars: usize,
    pub estimated_output_tokens: usize,
    pub chars_per_sec: f64,
    pub estimated_tokens_per_sec: f64,
}

impl BenchmarkStats {
    pub fn new(
        warmup_ms: Option<u128>,
        first_token_ms: Option<u128>,
        total_ms: u128,
        output_chars: usize,
    ) -> Self {
        let elapsed = (total_ms as f64 / 1000.0).max(0.001);
        let estimated_output_tokens = output_chars.div_ceil(4).max(1);
        Self {
            warmup_ms,
            first_token_ms,
            total_ms,
            output_chars,
            estimated_output_tokens,
            chars_per_sec: output_chars as f64 / elapsed,
            estimated_tokens_per_sec: estimated_output_tokens as f64 / elapsed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityRecord {
    pub generated_at: String,
    pub backend: String,
    pub base_url: String,
    pub model: String,
    pub models: CapabilityStatus,
    pub streaming: CapabilityStatus,
    pub usage_chunks: CapabilityStatus,
    pub tool_calls: CapabilityStatus,
    pub inline_tool_json: CapabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark: Option<BenchmarkStats>,
}

impl CapabilityRecord {
    pub fn minimal(backend_desc: &BackendDescriptor, model: &str) -> Self {
        Self {
            generated_at: Utc::now().to_rfc3339(),
            backend: backend_desc.name.as_str().into(),
            base_url: backend_desc.base_url.clone(),
            model: model.into(),
            models: CapabilityStatus::fail("not probed"),
            streaming: CapabilityStatus::fail("not probed"),
            usage_chunks: CapabilityStatus::fail("not probed"),
            tool_calls: CapabilityStatus::fail("not probed"),
            inline_tool_json: CapabilityStatus::fail("not probed"),
            warning: None,
            benchmark: None,
        }
    }

    pub fn backend_name(&self) -> Option<BackendName> {
        BackendName::parse(&self.backend)
    }

    pub fn is_cloud(&self) -> bool {
        self.backend == BackendName::Openrouter.as_str()
    }

    pub fn tool_path(&self) -> &'static str {
        if self.tool_calls.ok {
            "native"
        } else if self.inline_tool_json.ok {
            "inline-json"
        } else {
            "none"
        }
    }
}

pub fn cache_dir(session_dir: &str) -> PathBuf {
    Path::new(session_dir).join("capabilities")
}

pub fn record_score(record: &CapabilityRecord) -> i64 {
    if !record.models.ok && !record.streaming.ok {
        return -1_000;
    }

    let mut score = 0i64;
    if record.models.ok {
        score += 80;
    }
    if record.streaming.ok {
        score += 320;
    }
    if record.usage_chunks.ok {
        score += 30;
    }
    if record.tool_calls.ok {
        score += 260;
    } else if record.inline_tool_json.ok {
        score += 150;
    }
    if record.warning.is_none() {
        score += 30;
    } else {
        score -= 80;
    }
    if !record.is_cloud() {
        score += 40;
    }

    if let Some(bench) = &record.benchmark {
        score += bench.estimated_tokens_per_sec.round().min(250.0) as i64;
        match bench.first_token_ms {
            Some(ms) if ms <= 1_500 => score += 90,
            Some(ms) if ms <= 4_000 => score += 45,
            Some(ms) if ms >= 10_000 => score -= 45,
            Some(_) => {}
            None => score -= 10,
        }
    }

    score
}

pub fn recommended_tool_selection(_record: &CapabilityRecord) -> ToolSelection {
    ToolSelection::Auto
}

pub fn warmup_recommended(record: &CapabilityRecord) -> bool {
    record
        .benchmark
        .as_ref()
        .and_then(|bench| bench.first_token_ms)
        .map(|ms| ms >= 1_500)
        .unwrap_or(true)
}

pub fn best_record(records: &[CapabilityRecord], include_cloud: bool) -> Option<CapabilityRecord> {
    records
        .iter()
        .filter(|record| include_cloud || !record.is_cloud())
        .filter(|record| record.streaming.ok)
        .max_by(|a, b| compare_records(a, b))
        .cloned()
}

pub fn sorted_records(records: &[CapabilityRecord]) -> Vec<CapabilityRecord> {
    let mut out = records.to_vec();
    out.sort_by(|a, b| compare_records(b, a));
    out
}

pub fn load_records(session_dir: &str) -> Result<Vec<CapabilityRecord>> {
    let dir = cache_dir(session_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        if let Ok(record) = serde_json::from_str::<CapabilityRecord>(&text) {
            records.push(record);
        }
    }
    Ok(sorted_records(&records))
}

pub fn save_record(session_dir: &str, mut record: CapabilityRecord) -> Result<PathBuf> {
    let dir = cache_dir(session_dir);
    fs::create_dir_all(&dir)?;
    let path = record_path(&dir, &record.backend, &record.base_url, &record.model);
    if record.benchmark.is_none() {
        if let Some(existing) = load_record_path(&path)? {
            record.benchmark = existing.benchmark;
        }
    }
    fs::write(&path, serde_json::to_string_pretty(&record)? + "\n")?;
    Ok(path)
}

pub fn save_benchmark(
    session_dir: &str,
    backend_desc: &BackendDescriptor,
    model: &str,
    benchmark: BenchmarkStats,
) -> Result<PathBuf> {
    let dir = cache_dir(session_dir);
    fs::create_dir_all(&dir)?;
    let path = record_path(
        &dir,
        backend_desc.name.as_str(),
        &backend_desc.base_url,
        model,
    );
    let mut record = load_record_path(&path)?.unwrap_or_else(|| CapabilityRecord {
        streaming: CapabilityStatus::ok("bench stream completed"),
        ..CapabilityRecord::minimal(backend_desc, model)
    });
    record.generated_at = Utc::now().to_rfc3339();
    record.benchmark = Some(benchmark);
    if !record.streaming.ok {
        record.streaming = CapabilityStatus::ok("bench stream completed");
    }
    fs::write(&path, serde_json::to_string_pretty(&record)? + "\n")?;
    Ok(path)
}

fn compare_records(a: &CapabilityRecord, b: &CapabilityRecord) -> Ordering {
    record_score(a)
        .cmp(&record_score(b))
        .then_with(|| a.backend.cmp(&b.backend))
        .then_with(|| a.model.cmp(&b.model))
}

fn load_record_path(path: &Path) -> Result<Option<CapabilityRecord>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str::<CapabilityRecord>(&text).ok())
}

fn record_path(dir: &Path, backend: &str, base_url: &str, model: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    base_url.hash(&mut hasher);
    let base_hash = hasher.finish();
    dir.join(format!(
        "{}--{}--{:016x}.json",
        sanitize_component(backend),
        sanitize_component(model),
        base_hash
    ))
}

fn sanitize_component(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "unknown".into()
    } else {
        trimmed.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: BackendName) -> BackendDescriptor {
        BackendDescriptor {
            name,
            base_url: "http://localhost:11434/v1".into(),
            api_key: "test".into(),
            is_local: true,
        }
    }

    fn record(backend: &str, model: &str, tool_calls: bool, tps: Option<f64>) -> CapabilityRecord {
        CapabilityRecord {
            generated_at: "2026-04-27T00:00:00Z".into(),
            backend: backend.into(),
            base_url: "http://localhost:11434/v1".into(),
            model: model.into(),
            models: CapabilityStatus::ok("ok"),
            streaming: CapabilityStatus::ok("ok"),
            usage_chunks: CapabilityStatus::fail("no usage"),
            tool_calls: CapabilityStatus {
                ok: tool_calls,
                detail: "tool probe".into(),
            },
            inline_tool_json: CapabilityStatus::fail("no inline"),
            warning: None,
            benchmark: tps.map(|estimated_tokens_per_sec| BenchmarkStats {
                warmup_ms: Some(100),
                first_token_ms: Some(900),
                total_ms: 1_000,
                output_chars: 40,
                estimated_output_tokens: 10,
                chars_per_sec: estimated_tokens_per_sec * 4.0,
                estimated_tokens_per_sec,
            }),
        }
    }

    #[test]
    fn benchmark_estimates_tokens_per_second() {
        let stats = BenchmarkStats::new(Some(100), Some(250), 2_000, 80);
        assert_eq!(stats.estimated_output_tokens, 20);
        assert!((stats.estimated_tokens_per_sec - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn local_records_win_by_default_over_cloud() {
        let local = record("ollama", "local", true, Some(20.0));
        let cloud = record("openrouter", "cloud", true, Some(200.0));
        let records = vec![local.clone(), cloud.clone()];

        assert_eq!(
            best_record(&records, false).map(|r| r.model),
            Some(local.model)
        );
        assert_eq!(
            best_record(&records, true).map(|r| r.model),
            Some(cloud.model)
        );
    }

    #[test]
    fn saves_and_loads_record_with_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        let backend = descriptor(BackendName::Ollama);
        let mut rec = CapabilityRecord::minimal(&backend, "qwen2.5-coder:7b");
        rec.models = CapabilityStatus::ok("1 model");
        rec.streaming = CapabilityStatus::ok("streaming");

        save_record(dir.path().to_str().unwrap(), rec).unwrap();
        save_benchmark(
            dir.path().to_str().unwrap(),
            &backend,
            "qwen2.5-coder:7b",
            BenchmarkStats::new(None, Some(500), 1_000, 40),
        )
        .unwrap();

        let loaded = load_records(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].benchmark.is_some());
        assert!(loaded[0].streaming.ok);
    }

    #[test]
    fn sanitizes_path_components() {
        let path = record_path(Path::new("/tmp"), "llama.cpp", "http://x", "repo/model:7b");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("llama.cpp--repo-model-7b--"));
        assert!(name.ends_with(".json"));
    }
}

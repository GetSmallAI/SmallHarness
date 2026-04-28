use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareSpec {
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chip_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_cpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_cpus: Option<u32>,
}

impl HardwareSpec {
    pub fn current_platform() -> Self {
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            chip_name: None,
            machine_name: None,
            memory_bytes: None,
            physical_cpus: None,
            logical_cpus: std::thread::available_parallelism()
                .ok()
                .map(|n| n.get() as u32),
        }
    }

    pub fn tier(&self) -> HardwareTier {
        classify_memory(self.memory_bytes)
    }

    pub fn recommended_profile(&self) -> &'static str {
        self.tier().recommended_profile()
    }

    pub fn memory_gb(&self) -> Option<f32> {
        self.memory_bytes
            .map(|bytes| bytes as f32 / 1024.0 / 1024.0 / 1024.0)
    }

    pub fn memory_label(&self) -> String {
        self.memory_gb()
            .map(|gb| format!("{gb:.0} GB"))
            .unwrap_or_else(|| "unknown memory".into())
    }

    fn merge_patch(&mut self, patch: HardwarePatch) {
        if self.chip_name.is_none() {
            self.chip_name = patch.chip_name;
        }
        if self.machine_name.is_none() {
            self.machine_name = patch.machine_name;
        }
        if self.memory_bytes.is_none() {
            self.memory_bytes = patch.memory_bytes;
        }
        if self.physical_cpus.is_none() {
            self.physical_cpus = patch.physical_cpus;
        }
        if self.logical_cpus.is_none() {
            self.logical_cpus = patch.logical_cpus;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HardwareTier {
    Tiny,
    Small,
    Medium,
    Large,
    Unknown,
}

impl HardwareTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            HardwareTier::Tiny => "tiny",
            HardwareTier::Small => "small",
            HardwareTier::Medium => "medium",
            HardwareTier::Large => "large",
            HardwareTier::Unknown => "unknown",
        }
    }

    pub fn recommended_profile(&self) -> &'static str {
        match self {
            HardwareTier::Tiny | HardwareTier::Small | HardwareTier::Unknown => "mac-mini-16gb",
            HardwareTier::Medium | HardwareTier::Large => "mac-studio-32gb",
        }
    }

    pub fn guidance(&self) -> &'static str {
        match self {
            HardwareTier::Tiny => {
                "local coding models may be constrained; prefer tiny or cloud fallback"
            }
            HardwareTier::Small => "prefer 7B/8B quantized coding models",
            HardwareTier::Medium => "prefer 14B quantized coding models",
            HardwareTier::Large => "larger local coding models are reasonable when installed",
            HardwareTier::Unknown => {
                "hardware memory is unknown; recommendations use conservative defaults"
            }
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct HardwarePatch {
    chip_name: Option<String>,
    machine_name: Option<String>,
    memory_bytes: Option<u64>,
    physical_cpus: Option<u32>,
    logical_cpus: Option<u32>,
}

pub fn detect_hardware_spec() -> HardwareSpec {
    let mut spec = HardwareSpec::current_platform();
    spec.merge_patch(sysctl_patch());

    if spec.os == "macos" {
        if let Some(text) = command_output("system_profiler", &["SPHardwareDataType", "-json"]) {
            spec.merge_patch(parse_system_profiler_hardware_json(&text));
        }
    }

    spec
}

pub fn hardware_cache_path(session_dir: &str) -> PathBuf {
    Path::new(session_dir).join("hardware.json")
}

pub fn save_hardware_summary(session_dir: &str, spec: &HardwareSpec) -> Result<PathBuf> {
    fs::create_dir_all(session_dir)?;
    let path = hardware_cache_path(session_dir);
    fs::write(&path, serde_json::to_string_pretty(spec)? + "\n")?;
    Ok(path)
}

pub fn classify_memory(memory_bytes: Option<u64>) -> HardwareTier {
    let Some(bytes) = memory_bytes else {
        return HardwareTier::Unknown;
    };
    let gb = bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    if gb < 12.0 {
        HardwareTier::Tiny
    } else if gb < 24.0 {
        HardwareTier::Small
    } else if gb < 48.0 {
        HardwareTier::Medium
    } else {
        HardwareTier::Large
    }
}

fn sysctl_patch() -> HardwarePatch {
    parse_sysctl_values(
        sysctl_value("hw.memsize").as_deref(),
        sysctl_value("hw.physicalcpu").as_deref(),
        sysctl_value("hw.logicalcpu").as_deref(),
        sysctl_value("machdep.cpu.brand_string").as_deref(),
    )
}

fn sysctl_value(key: &str) -> Option<String> {
    command_output("sysctl", &["-n", key]).map(|s| s.trim().to_string())
}

fn command_output(cmd: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(cmd).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_sysctl_values(
    memory: Option<&str>,
    physical: Option<&str>,
    logical: Option<&str>,
    chip: Option<&str>,
) -> HardwarePatch {
    HardwarePatch {
        memory_bytes: memory.and_then(|s| s.trim().parse::<u64>().ok()),
        physical_cpus: physical.and_then(|s| s.trim().parse::<u32>().ok()),
        logical_cpus: logical.and_then(|s| s.trim().parse::<u32>().ok()),
        chip_name: chip
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        machine_name: None,
    }
}

fn parse_system_profiler_hardware_json(text: &str) -> HardwarePatch {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return HardwarePatch::default();
    };
    let Some(entry) = value
        .get("SPHardwareDataType")
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
    else {
        return HardwarePatch::default();
    };

    let chip_name = entry
        .get("chip_type")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let machine_name = entry
        .get("machine_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let memory_bytes = entry
        .get("physical_memory")
        .and_then(|v| v.as_str())
        .and_then(parse_memory_label);
    let logical_cpus = entry
        .get("number_processors")
        .and_then(|v| v.as_str())
        .and_then(parse_number_processors);

    HardwarePatch {
        chip_name,
        machine_name,
        memory_bytes,
        physical_cpus: None,
        logical_cpus,
    }
}

fn parse_number_processors(s: &str) -> Option<u32> {
    let raw = s.trim().strip_prefix("proc ")?;
    raw.split(':').next()?.parse::<u32>().ok()
}

fn parse_memory_label(s: &str) -> Option<u64> {
    let mut parts = s.split_whitespace();
    let value = parts.next()?.parse::<f64>().ok()?;
    let unit = parts.next()?.to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "kb" | "kib" => 1024.0,
        _ => return None,
    };
    Some((value * multiplier) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gb(n: u64) -> u64 {
        n * 1024 * 1024 * 1024
    }

    #[test]
    fn classifies_memory_boundaries() {
        assert_eq!(classify_memory(Some(gb(8))), HardwareTier::Tiny);
        assert_eq!(classify_memory(Some(gb(12))), HardwareTier::Small);
        assert_eq!(classify_memory(Some(gb(23))), HardwareTier::Small);
        assert_eq!(classify_memory(Some(gb(24))), HardwareTier::Medium);
        assert_eq!(classify_memory(Some(gb(47))), HardwareTier::Medium);
        assert_eq!(classify_memory(Some(gb(48))), HardwareTier::Large);
        assert_eq!(classify_memory(None), HardwareTier::Unknown);
    }

    #[test]
    fn parses_sysctl_fallback_values() {
        let patch = parse_sysctl_values(
            Some("17179869184"),
            Some("10"),
            Some("10"),
            Some("Apple M4"),
        );
        assert_eq!(patch.memory_bytes, Some(gb(16)));
        assert_eq!(patch.physical_cpus, Some(10));
        assert_eq!(patch.logical_cpus, Some(10));
        assert_eq!(patch.chip_name.as_deref(), Some("Apple M4"));
    }

    #[test]
    fn parses_system_profiler_without_sensitive_fields() {
        let text = r#"{
          "SPHardwareDataType": [{
            "chip_type": "Apple M4",
            "machine_name": "Mac mini",
            "physical_memory": "16 GB",
            "number_processors": "proc 10:4:6:0",
            "serial_number": "SECRET-SERIAL",
            "platform_UUID": "SECRET-UUID",
            "provisioning_UDID": "SECRET-UDID",
            "activation_lock_status": "activation_lock_enabled"
          }]
        }"#;
        let patch = parse_system_profiler_hardware_json(text);
        let mut spec = HardwareSpec {
            os: "macos".into(),
            arch: "arm64".into(),
            chip_name: None,
            machine_name: None,
            memory_bytes: None,
            physical_cpus: None,
            logical_cpus: None,
        };
        spec.merge_patch(patch);
        let serialized = serde_json::to_string(&spec).unwrap();

        assert_eq!(spec.chip_name.as_deref(), Some("Apple M4"));
        assert_eq!(spec.machine_name.as_deref(), Some("Mac mini"));
        assert_eq!(spec.memory_bytes, Some(gb(16)));
        assert_eq!(spec.logical_cpus, Some(10));
        assert!(!serialized.contains("SECRET"));
        assert!(!serialized.contains("serial"));
        assert!(!serialized.contains("UDID"));
    }

    #[test]
    fn maps_tiers_to_existing_profiles() {
        assert_eq!(HardwareTier::Tiny.recommended_profile(), "mac-mini-16gb");
        assert_eq!(HardwareTier::Small.recommended_profile(), "mac-mini-16gb");
        assert_eq!(
            HardwareTier::Medium.recommended_profile(),
            "mac-studio-32gb"
        );
        assert_eq!(HardwareTier::Large.recommended_profile(), "mac-studio-32gb");
    }
}

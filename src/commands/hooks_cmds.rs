use anyhow::{anyhow, Result};
use std::path::PathBuf;

use crate::app_state::AppState;
use crate::hooks::{
    hook_state_file_path, load_hook_state_file_from, save_hook_state_file_to, HookSourceKind,
    HookState, HookStateFile, HookTrustStatus,
};

use super::{DIM, GREEN, RED, RESET, YELLOW};

pub(super) fn cmd_hooks(args: &str, state: &mut AppState) -> Result<()> {
    let mut parts = args.split_whitespace();
    match parts.next() {
        None | Some("") | Some("list") => {
            print_hooks(state);
            Ok(())
        }
        Some("trust") => {
            let key = parts
                .next()
                .ok_or_else(|| anyhow!("Usage: /hooks trust <key>"))?;
            trust_hook(state, key)
        }
        Some("trust-all") => trust_all_hooks(state),
        Some("disable") => {
            let key = parts
                .next()
                .ok_or_else(|| anyhow!("Usage: /hooks disable <key>"))?;
            set_hook_enabled(state, key, false)
        }
        Some("enable") => {
            let key = parts
                .next()
                .ok_or_else(|| anyhow!("Usage: /hooks enable <key>"))?;
            set_hook_enabled(state, key, true)
        }
        Some(_) => {
            println!(
                "  {DIM}Usage: /hooks [list|trust <key>|trust-all|disable <key>|enable <key>]{RESET}"
            );
            Ok(())
        }
    }
}

fn print_hooks(state: &AppState) {
    if state.hooks.entries.is_empty() {
        println!("  {DIM}No hooks configured.{RESET}");
        return;
    }
    println!("  {GREEN}Hooks{RESET}");
    for entry in &state.hooks.entries {
        println!(
            "  {:>2}. {} {:<16} {:<13} {}",
            entry.display_order + 1,
            status_label(entry.trust_status),
            entry.event.as_str(),
            source_label(entry.source.kind.clone()),
            entry.key
        );
        let matcher = entry.matcher.as_deref().unwrap_or("*");
        println!(
            "      {DIM}matcher={matcher} command={} hash={}{RESET}",
            entry.handler.command, entry.current_hash
        );
        if let Some(error) = &entry.matcher_error {
            println!("      {YELLOW}!{RESET} {DIM}{error}{RESET}");
        }
    }
}

fn trust_hook(state: &mut AppState, key: &str) -> Result<()> {
    let Some((hash, is_managed, is_invalid)) = state
        .hooks
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| {
            (
                entry.current_hash.clone(),
                entry.source.kind == HookSourceKind::ManagedLaunch,
                entry.trust_status == HookTrustStatus::Invalid,
            )
        })
    else {
        return Err(anyhow!("unknown hook key: {key}"));
    };
    if is_invalid {
        println!("  {DIM}Invalid hooks cannot be trusted until their matcher is fixed.{RESET}");
        return Ok(());
    }
    if is_managed {
        println!("  {DIM}Managed launch hooks are already trusted for this process.{RESET}");
        return Ok(());
    }
    let (path, mut file) = load_user_hook_state()?;
    file.hooks.insert(
        key.to_string(),
        HookState {
            enabled: Some(true),
            trusted_hash: Some(hash),
        },
    );
    save_hook_state_file_to(&path, &file)?;
    set_runtime_status(state, key, HookTrustStatus::Trusted);
    println!("  {GREEN}✓{RESET} {DIM}trusted hook {key}{RESET}");
    Ok(())
}

fn trust_all_hooks(state: &mut AppState) -> Result<()> {
    let candidates: Vec<(String, String)> = state
        .hooks
        .entries
        .iter()
        .filter(|entry| entry.source.kind != HookSourceKind::ManagedLaunch)
        .filter(|entry| {
            matches!(
                entry.trust_status,
                HookTrustStatus::Untrusted | HookTrustStatus::Modified
            )
        })
        .map(|entry| (entry.key.clone(), entry.current_hash.clone()))
        .collect();
    if candidates.is_empty() {
        println!("  {DIM}No hooks need trust review.{RESET}");
        return Ok(());
    }
    let (path, mut file) = load_user_hook_state()?;
    for (key, hash) in &candidates {
        file.hooks.insert(
            key.clone(),
            HookState {
                enabled: Some(true),
                trusted_hash: Some(hash.clone()),
            },
        );
    }
    save_hook_state_file_to(&path, &file)?;
    for (key, _) in &candidates {
        set_runtime_status(state, key, HookTrustStatus::Trusted);
    }
    println!(
        "  {GREEN}✓{RESET} {DIM}trusted {} hook(s){RESET}",
        candidates.len()
    );
    Ok(())
}

fn set_hook_enabled(state: &mut AppState, key: &str, enabled: bool) -> Result<()> {
    let Some(entry) = state
        .hooks
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .cloned()
    else {
        return Err(anyhow!("unknown hook key: {key}"));
    };
    if entry.source.kind == HookSourceKind::ManagedLaunch {
        println!("  {DIM}Managed launch hooks cannot be changed from /hooks.{RESET}");
        return Ok(());
    }
    if entry.trust_status == HookTrustStatus::Invalid {
        println!("  {DIM}Invalid hooks cannot be changed until their matcher is fixed.{RESET}");
        return Ok(());
    }
    let (path, mut file) = load_user_hook_state()?;
    let hook_state = file.hooks.entry(key.to_string()).or_default();
    hook_state.enabled = Some(enabled);
    let status = status_from_state(hook_state, &entry.current_hash);
    save_hook_state_file_to(&path, &file)?;
    set_runtime_status(state, key, status);
    let verb = if enabled { "enabled" } else { "disabled" };
    println!("  {GREEN}✓{RESET} {DIM}{verb} hook {key}{RESET}");
    Ok(())
}

fn load_user_hook_state() -> Result<(PathBuf, HookStateFile)> {
    let path = hook_state_file_path()
        .ok_or_else(|| anyhow!("HOME or XDG_CONFIG_HOME is required to save hook state"))?;
    let file = load_hook_state_file_from(&path)?;
    Ok((path, file))
}

fn set_runtime_status(state: &mut AppState, key: &str, status: HookTrustStatus) {
    if let Some(entry) = state
        .hooks
        .entries
        .iter_mut()
        .find(|entry| entry.key == key)
    {
        entry.trust_status = status;
    }
    refresh_runnable(state);
}

fn refresh_runnable(state: &mut AppState) {
    state.hooks.runnable = state
        .hooks
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.trust_status,
                HookTrustStatus::Managed | HookTrustStatus::Trusted
            )
        })
        .cloned()
        .collect();
}

fn status_from_state(state: &HookState, current_hash: &str) -> HookTrustStatus {
    if state.enabled == Some(false) {
        return HookTrustStatus::Disabled;
    }
    match state.trusted_hash.as_deref() {
        Some(hash) if hash == current_hash => HookTrustStatus::Trusted,
        Some(_) => HookTrustStatus::Modified,
        None => HookTrustStatus::Untrusted,
    }
}

fn status_label(status: HookTrustStatus) -> String {
    match status {
        HookTrustStatus::Managed => format!("{GREEN}[managed]{RESET}"),
        HookTrustStatus::Trusted => format!("{GREEN}[trusted]{RESET}"),
        HookTrustStatus::Modified => format!("{YELLOW}[modified]{RESET}"),
        HookTrustStatus::Untrusted => format!("{YELLOW}[new]{RESET}"),
        HookTrustStatus::Disabled => format!("{RED}[disabled]{RESET}"),
        HookTrustStatus::Invalid => format!("{RED}[invalid]{RESET}"),
    }
}

fn source_label(kind: HookSourceKind) -> &'static str {
    match kind {
        HookSourceKind::User => "user",
        HookSourceKind::Project => "project",
        HookSourceKind::ManagedLaunch => "managed",
    }
}

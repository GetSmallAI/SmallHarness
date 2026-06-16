use futures_util::stream::{self, StreamExt};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

use super::{
    discover_hooks, matcher_matches, run_command_hook, DiscoveredHook, HookConfig, HookDiscovery,
    HookEventName, HookRunResult, HookSource, HookStateFile, HookStateStore, ManagedHookConfig,
};

const MAX_CONCURRENT_HOOKS: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct HookRegistry {
    pub entries: Vec<DiscoveredHook>,
    pub runnable: Vec<DiscoveredHook>,
}

#[derive(Debug, Clone, Default)]
pub struct HookDispatch {
    pub results: Vec<HookDispatchResult>,
}

#[derive(Debug, Clone)]
pub struct HookDispatchResult {
    pub hook: DiscoveredHook,
    pub run: HookRunResult,
}

impl HookRegistry {
    pub fn from_discoveries(discoveries: Vec<HookDiscovery>) -> Self {
        let mut registry = HookRegistry::default();
        for discovery in discoveries {
            registry.entries.extend(discovery.entries);
            registry.runnable.extend(discovery.runnable);
        }
        registry
    }

    pub fn matching_hooks(
        &self,
        event: HookEventName,
        matcher_value: Option<&str>,
    ) -> Vec<DiscoveredHook> {
        let matcher_value =
            matcher_value.or_else(|| event.supports_matcher().then_some(event.as_str()));
        self.runnable
            .iter()
            .filter(|hook| hook.event == event)
            .filter(|hook| matcher_matches(hook.matcher.as_deref(), matcher_value))
            .cloned()
            .collect()
    }

    pub async fn dispatch(
        &self,
        event: HookEventName,
        payload: &Value,
        matcher_value: Option<&str>,
    ) -> HookDispatch {
        let hooks = self.matching_hooks(event, matcher_value);
        let mut runs = stream::iter(hooks.iter().cloned().enumerate())
            .map(|(idx, hook)| async move { (idx, run_command_hook(&hook.handler, payload).await) })
            .buffer_unordered(MAX_CONCURRENT_HOOKS)
            .collect::<Vec<_>>()
            .await;
        runs.sort_by_key(|(idx, _)| *idx);
        HookDispatch {
            results: hooks
                .into_iter()
                .zip(runs.into_iter().map(|(_, run)| run))
                .map(|(hook, run)| HookDispatchResult { hook, run })
                .collect(),
        }
    }
}

pub fn build_hook_registry(
    project_hooks: &HookConfig,
    managed_hooks: Option<&ManagedHookConfig>,
    user_state: &HookStateFile,
    project_root: &str,
) -> HookRegistry {
    let state_store = HookStateStore {
        user: user_state.hooks.clone(),
    };
    let mut discoveries = vec![discover_hooks(
        project_hooks,
        HookSource::project(project_hook_source_label(project_root)),
        &state_store,
    )];
    if let Some(managed) = managed_hooks {
        discoveries.push(discover_hooks(
            &managed.hooks,
            HookSource::managed_launch(managed.source_label.clone()),
            &HookStateStore::default(),
        ));
    }
    HookRegistry::from_discoveries(discoveries)
}

fn project_hook_source_label(project_root: &str) -> String {
    let root = Path::new(project_root);
    lexical_normalize_path(root)
        .join("agent.config.json")
        .display()
        .to_string()
}

fn lexical_normalize_path(path: &Path) -> PathBuf {
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

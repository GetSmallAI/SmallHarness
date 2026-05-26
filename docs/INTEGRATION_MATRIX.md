# Integration matrix

Run this **before marking any plan/feature complete**. Unit tests alone are not sufficient when work touches config, session state, operator modes, tools, or the filesystem.

---

## Quick start

This file is the **checklist** (G/F/P rows below). It is not what you paste into chat.

| When | What to do |
|------|------------|
| **Starting a plan** | Open [`docs/PLAN_KICKOFF.txt`](PLAN_KICKOFF.txt), copy all of it, paste above your plan in chat |
| **Reviewing a finished plan** | Skim the agent's **Completion report** (template below) |

### Completion report (agent fills this in at the end)

The agent must paste this filled in before you merge:

```
## Integration matrix — Session Paths

Global: G1 [x] G2 [x] G3 [x] G4 [x] G5 [x] G6 [x]
Feature: F1 [x] F2 [x] F3 [x] F4 [x] F5 [ ] F6 [ ] F7 [ ] F8 [x] (N/A where unchecked)

Plan-specific:
| P1 | Fork → mutate → switch → workspace round-trips with workspaceRoot "." | pass | test: session_paths.rs::fork_switch_round_trip |
| P2 | /path pick uses approval preview; dry-run does not write | pass | test: session_paths.rs::pick_dry_run_does_not_write |
| P3 | /new clears path registry; resume restores active path id | pass | test: commands.rs::new_resets_path_store_for_fresh_session |
| P4 | Refuse fork/switch/pick during active /play | pass | test: commands.rs::path_fork_refuses_during_play_session |
| P5 | Pick result parsing uses structured applied field | pass | test: session_paths.rs::pick_applies_structured_result |
| P6 | Untracked file created on branch A absent after switch to main | pass | test: session_paths.rs::fork_switch_round_trip |

Commands: cargo test / fmt / clippy — all green
Follow-ups (if any): none
```

**Your job at the end:** skim the report (~2 min). If P-rows or G-rows are blank but the feature clearly touches those areas, send it back.

---

## Global baseline (every build)

| # | Scenario | How to verify | Automated test? |
|---|----------|---------------|-----------------|
| G1 | Default config shape | Use `workspaceRoot: "."` (or project default), not only absolute tempdirs | Required if feature touches paths |
| G2 | Dual flags | If both `config.*` and `AppState.*` exist, sync on `/mode`, `/new`, presets, and toggles | Required if feature adds session override |
| G3 | Path normalization | Never re-check `starts_with(raw_root)` after a normalizing resolver; trust one helper | Required if feature adds path jail |
| G4 | Dry-run / no-op tools | Previews and dry-runs must not trigger side effects (memory refresh, checkpoints, auto-verify) | Required if feature wraps mutating tools |
| G5 | Operator mode switch | explore → edit/ship enables feature; reverse disables if documented | Required if feature is mode-gated |
| G6 | Tool result parsing | Gate on structured JSON fields, not `"error"` substring alone | Required if feature reacts to tool output |

---

## Feature-specific (check when applicable)

### Filesystem / checkpoints / undo

| # | Scenario | How to verify |
|---|----------|---------------|
| F1 | Relative `workspaceRoot` | Snapshot + restore with `"."` while cwd is temp workspace |
| F2 | New file created by agent | Undo deletes file |
| F3 | Untracked file edited | Lazy snapshot before first write |
| F4 | `batch_edit` dry-run | No checkpoint, no mutation flags |

### Ship loop / tests

| # | Scenario | How to verify |
|---|----------|---------------|
| F5 | Untracked new file | Smart test selection includes `git ls-files --others` paths |
| F6 | Post-turn auto-verify | Only after real mutations; failures injected to context |

### Context / session

| # | Scenario | How to verify |
|---|----------|---------------|
| F7 | Mid-run compaction | Tool-call spans stay intact; summary persists |
| F8 | `/new` / `/resume` | Session-scoped state cleared or documented |

---

## Current build

_Plan name: Session Paths  
Date: 2026-05-25

### Plan-specific rows

| # | Scenario | Pass? | Test added (path) |
|---|----------|-------|-------------------|
| P1 | Fork → mutate → switch → workspace round-trips with `workspaceRoot: "."` | [x] | `session_paths.rs::fork_switch_round_trip`, `relative_workspace_root_capture_and_restore` |
| P2 | `/path pick` dry-run does not write; live pick uses approval in `cmd_path` | [x] | `session_paths.rs::pick_dry_run_does_not_write`, `pick_applies_structured_result` |
| P3 | `/new` resets path store; `/resume` + `--continue` load `activePathId` | [x] | `commands.rs::new_resets_path_store_for_fresh_session` |
| P4 | Refuse `/path` fork during active `/play` | [x] | `commands.rs::path_fork_refuses_during_play_session` |
| P5 | Pick uses structured `applied` field, not `"error"` substring | [x] | `session_paths.rs::pick_applies_structured_result` |
| P6 | Branch-only file removed when switching back to main | [x] | `session_paths.rs::fork_switch_round_trip` |

### Global baseline

- [x] G1  [x] G2  [x] G3  [x] G4  [x] G5  [x] G6

Notes:
- G1/G3: `session_paths.rs::relative_workspace_root_capture_and_restore`, `resolve_under_workspace`
- G2: paths are config-only (`paths.enabled`); no dual session toggle — `/new` resets `PathStore`
- G4: `/path pick --dry-run` and `PickResult { dry_run, applied }` do not write files
- G5: paths work in all operator modes; pick always approval-gated
- G6: `PickResult.applied` gates success in tests and command handler

### Feature-specific (check all that apply)

- [x] F1  [x] F2  [x] F3  [x] F4  [ ] F5  [ ] F6  [ ] F7  [x] F8

Notes:
- F1/F3: `session_paths.rs::relative_workspace_root_capture_and_restore`, `untracked_file_captured`
- F2: branch file removal on switch uses snapshot diff (`remove_files_not_in_snapshot`)
- F4: N/A (paths are slash commands, not batch_edit)
- F8: `commands.rs::new_resets_path_store_for_fresh_session`; resume loads `activePathId` from session meta

### Commands run

```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

---

## Adding new rows

When a review finds a wiring bug, add a row here **and** a regression test. Prefer naming the failure mode ("relative workspaceRoot") over the file line number.

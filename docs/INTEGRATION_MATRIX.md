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
## Integration matrix — [plan name]

Global: G1 [ ] G2 [ ] G3 [ ] G4 [ ] G5 [ ] G6 [ ]
Feature: F1 [ ] F2 [ ] F3 [ ] F4 [ ] F5 [ ] F6 [ ] F7 [ ] F8 [ ] (N/A where unchecked)

Plan-specific:
| P1 | … | pass | test: … |
| P2 | … | pass | test: … |

Commands: cargo test / fmt / clippy — all green
Follow-ups (if any): …
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

_Plan name: Play and Fix Features  
Date: 2026-05-23

### Plan-specific rows

| # | Scenario | Pass? | Test added (path) |
|---|----------|-------|-------------------|
| P1 | `/play exit` restores original `workspace_root`, mode, and full mode-mutated config | [x] | `playground.rs::play_session_restore_round_trip` |
| P2 | `/play` refuses nested start; `/fix` refuses while play active | [x] | `playground.rs::play_refuses_while_active`, `fix_loop.rs::fix_refuses_during_play_session` |
| P3 | `/fix` stops when tests pass; respects `--attempts N` and `--attempts=N`; restores full mode-mutated config | [x] | `fix_loop.rs::fix_loop_stops_when_tests_pass`, `fix_loop.rs::fix_mode_restore_round_trip`, `fix_loop.rs::fix_loop_respects_max_attempts_config` |
| P4 | `/play battle` produces comparison table + JSON export | [x] | `playground.rs::play_battle_saves_json_and_markdown`, `run_play_battle` wiring |
| P5 | `/fix` sets `auto_verify_tests: false` to avoid double test runs | [x] | `fix_loop.rs` (explicit re-run after each turn) |

### Global baseline

- [x] G1  [x] G2  [x] G3  [x] G4  [x] G5  [x] G6

Notes:
- G1/G3: `turn_checkpoint.rs::relative_workspace_root_restore_round_trip`, `turn_checkpoint.rs::snapshot_works_with_relative_workspace_root`
- G2/G5: `playground.rs::play_session_restore_round_trip`, `fix_loop.rs::fix_mode_restore_round_trip`, `commands.rs::mode_ship_syncs_session_checkpoints_flag`
- G4: `tools::tests::batch_edit_dry_run_does_not_count_as_mutation`, `turn_checkpoint.rs::batch_edit_dry_run_should_not_push`
- G6: `tools::tests::mutation_detection_uses_structured_fields_not_error_substrings`

### Feature-specific (check all that apply)

- [x] F1  [x] F2  [x] F3  [x] F4  [x] F5  [x] F6  [x] F7  [x] F8

Notes:
- F1/F2/F3: `turn_checkpoint.rs::relative_workspace_root_restore_round_trip`, `restore_removes_created_file`, `lazy_snapshot_before_untracked_edit`
- F4: `turn_checkpoint.rs::batch_edit_dry_run_should_not_push`
- F5: `test_integration.rs::smart_selection_includes_untracked_files`
- F6: `session_turn.rs` auto-verify path plus `tools::tests::file_edit_success_counts_as_mutation`
- F7: `context_guard.rs::partition_keeps_tool_rounds_intact`, `partition_can_split_large_single_user_turn_on_tool_boundaries`
- F8: `commands.rs::new_restores_play_session_and_clears_session_state`

### Commands run

```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

---

## Adding new rows

When a review finds a wiring bug, add a row here **and** a regression test. Prefer naming the failure mode ("relative workspaceRoot") over the file line number.

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.4.1] - 2026-06-03

A UI and onboarding polish pass.

### Added

- **TUI visual refresh.** A shared `theme` module drives a high-contrast
  palette and rounded-panel helpers. Input is now a framed `you` panel with an
  accent `❯` prompt; the assistant answer streams inside a framed `response`
  panel; tool calls use accent bullets, gutter-aligned. Crucially, secondary
  text no longer uses ANSI *faint* (the main reason the old TUI was hard to
  read) — it's readable bright-black.
- **API-key prompt in setup.** Choosing a cloud backend (`openai`/
  `openrouter`) in the wizard now prompts for the key and saves it to the
  `0600` auth file, so the end-of-setup probe no longer fails on a missing key.

### Changed

- The setup wizard no longer asks for a hardware profile (it keeps the
  existing/default profile silently).

### Docs

- README: a clear **Run it** section with the launch command, cloud and local
  presented as two equal paths, cleaner layout, and no hardcoded backend count.
- Moved internal AI-workflow checklists out of the public repo.

## [0.4.0] - 2026-06-03

A core-power + bare-bones pass: the agent loop got faster and more capable,
and the command/tool surface got leaner.

### Added

- **Parallel read-tool execution** — read-only tools (`file_read`, `grep`,
  `list_dir`, `glob`, `repo_search`, `ship_status`) emitted in a single step
  now run concurrently. Mutations and `shell`/`run_tests`/MCP stay serial;
  approval and checkpoint capture remain sequential.
- **`update_plan` tool** — the model maintains a short, visible task checklist
  across a multi-step turn (rendered as a plan box). No new slash commands; the
  plan lives in the conversation and auto-selection only surfaces it for
  multi-step work.
- **`task` subagent tool** — delegates a scoped, read-only investigation to a
  fresh agent loop (capped at 12 steps, no edit/shell, no recursion) and returns
  only its summary, keeping deep exploration out of the parent context.
- **Step-budget signalling** — when the loop exhausts `max_steps` mid-task it now
  sets `RunResult.hit_step_limit`, emits `StepLimitReached`, and shows a
  resumable notice instead of stopping silently. Eval results record it too.
- **Edit verification** — `file_edit` re-reads from disk after writing, returns
  `verified` (disk matches the intended write) and a line-numbered
  `applied_snippet` of the changed region so the model confirms the edit landed.
- **Session paths** — terminal-native branching without a worktree.
  `/path fork [name]` snapshots the workspace and clones the transcript;
  `/path switch <name>` restores another path; `/path diff` and `/path pick`
  compare and merge file changes with approval preview. State lives under
  `.sessions/paths/<session-id>/`. Config block `paths` controls enablement,
  max paths, and snapshot byte caps. Status line shows `path: name · N paths`
  when multiple paths exist. Resumes via `--continue`, `/resume`, and session
  metadata `activePathId`.

### Changed

- **Model-tuning commands folded under `/doctor`** — `/doctor` is now the single
  entry point with subcommands `recommend`, `autotune`, `bench`, and `models`.
  The old top-level `/recommend`, `/autotune`, `/bench`, and `/capabilities`
  print a one-line redirect and no longer appear in `/help`.
- **Default tool pool** is now `file_read`, `grep`, `list_dir`, `file_edit`,
  `shell`, `update_plan`, `task`. `shell` is on by default (still
  approval-gated); `repo_search` moved to opt-in.
- **`commands.rs` split** into `commands/{mod,doctor,workflow}.rs` with `dispatch`
  kept as a thin router. No behavior change.

## [0.3.0] - 2026-05-24

A polish + capability pass. Direct OpenAI provider, per-turn cost on the
status line, persistent credential store, an MCP client, image input,
`web_fetch`, a per-project system prompt, `--continue`, completions, a
GitHub release check, redacted crash logs, and a notarization-ready
release workflow with a Homebrew formula template.

### Added

- Direct OpenAI backend (`BackendName::OpenAi`, default model `gpt-4o-mini`,
  `OPENAI_API_KEY` / optional `OPENAI_BASE_URL`). `BackendName::is_local()`
  generalizes the cloud check so handoff refusal, recommend filtering, and
  capability scoring stay correct as cloud backends are added.
- Per-model catalog (`src/catalog.rs`) with context window and input/output
  USD-per-Mtoken for OpenAI's GPT-4o family, o-series, GPT-4, GPT-3.5.
  Lookup does exact-match then longest-prefix so versioned ids like
  `gpt-4o-2024-11-20` resolve to `gpt-4o`. `/model` picker shows
  `128k ctx · $0.15/$0.60 per Mtoken` alongside each id.
- Session cost tracker: end-of-turn status line shows `$0.003 this turn ·
  $0.41 session` on cloud backends. Local turns show tokens only. Sessions
  that mix cloud and uncataloged turns prefix the total with `≥` to mark
  it as a lower bound.
- `/auth` command + persistent credential store at
  `~/.config/small-harness/auth.json` (mode `0600`, masked display, env
  vars win at lookup time).
- `/image <path>` attaches images to the next user turn via OpenAI's
  multi-part content format. New `UserContent` enum is `#[serde(untagged)]`
  so plain text turns keep the existing wire shape. Catalog now tracks
  `vision: bool` per model.
- MCP client (`src/mcp.rs`) over stdio JSON-RPC with
  `initialize`/`tools/list`/`tools/call`. Servers configured under
  `mcpServers` in `agent.config.json` are spawned at startup and their
  tools surface as `mcp__<server>__<tool>`, approval-gated by default.
- `web_fetch` tool — approval-gated, HTML-stripped to text, byte-capped.
- Per-project system prompt: drop `.small-harness/prompt.md` at the repo
  root and Small Harness prepends it (auto-truncated at 8 KB).
- `small-harness --continue` (or `-c`) resumes the most recent session in
  the cwd without picking from a list.
- Background GitHub release check with a 24h cache; one-line banner notice
  if a newer version exists. Opt out with `SMALL_HARNESS_NO_UPDATE_CHECK`.
- Crash log: panic hook writes a redacted log (API keys scrubbed) to
  `.sessions/crashes/<timestamp>.log` and prints the path.
- `small-harness completions bash|zsh|fish` emits shell completion
  scripts.
- `/reasoning on|off|status` toggles the streaming reasoning panel with a
  proper "thinking…" header.
- GitHub Actions release workflow at `.github/workflows/release.yml` —
  tag-push triggered, dual-arch macOS build, optional sign + notarize when
  Apple Developer secrets are present, GitHub release with SHA256SUMS.
- Homebrew formula template at `packaging/homebrew/small-harness.rb`
  targeting that workflow's artifacts, ready to drop into a
  `getsmallai/homebrew-tap` repo.

### Changed

- Refactored `BackendName::Openrouter`-as-cloud checks in `handoff.rs`,
  `recommend.rs`, `capabilities.rs`, and `commands.rs` to route through
  `BackendName::is_local()` instead.
- `/new` now resets `total_in`, `total_out`, `session_usd`, and the
  uncataloged-cost flag so a fresh session genuinely starts fresh.
- Tagline and masthead updated: "A small, terminal-first coding harness —
  bring your own model, your own key, or your own MCP server." README
  fully restructured around install → first session → reference flow.

## [0.2.x predecessors]

Earlier `0.2.x` work that landed before 0.3.0 includes: interactive
`/play` playground with bundled demos and scorecards, `/fix` fix-until-green
loop, shared `session_turn.rs` runner, agent workflow tools (`run_tests`,
`batch_edit`, `ship_status`), `/mode ship` autopilot, agent eval suite,
turn checkpoints with `/undo`, multi-file `/batch` and `/refactor`, the
`/test` command, the `/prompt` template library, and the adaptive context
guard with auto-compaction.

## [0.2.2] - 2026-05-03

### Added

- `/handoff [export|save] [path] [--cloud]` for local model-drafted commit
  messages, changelog bullets, testing notes, and X-ready release copy from the
  current git branch or working tree.

### Changed

- Fixed the Quick Install clone URL to point at `GetSmallAI/SmallHarness`.

## [0.2.1] - 2026-05-02

### Added

- `/shipcheck [export [path]]` release preflight for git branch drift, staged
  and unstaged changes, untracked files, conflicts, diff stats, and
  project-memory freshness reports.

## [0.2.0] - 2026-04-29

### Added

- Project memory freshness checks with `/index status`, changed-file refresh,
  and automatic refresh after successful file mutation tools.
- Operator modes with `/mode explore`, `/mode edit`, `/mode ship`, and
  `/mode review` presets for tool sets, approvals, and step budgets.
- Session lifecycle improvements: inferred/settable titles, `/sessions search`,
  confirmed session deletion, and safe pruning of old sessions.
- Non-interactive one-shot mode via `--print`, `-p`, or piped stdin, with
  explicit `--allow-tools` approval for write/shell tool execution.
- Reasoning stream support for OpenAI-compatible chunks that expose
  `reasoning`, `reasoning_content`, or `thinking` deltas.

### Changed

- Bumped the product line to `0.2.0` for the broader release scope.

### Tests

- Added mock OpenAI-compatible SSE coverage for streamed tool-call deltas.

## [0.1.34] - 2026-04-29

### Added

- Local project memory under `.sessions/project-memory/`, with a metadata-only
  repo index that honors `.gitignore`, skips secret/env files, binaries,
  oversized files, `.git`, `.sessions`, `target`, and `node_modules`.
- `/index`, `/map`, `/memory`, `/remember`, and `/forget` commands for building,
  inspecting, toggling, and annotating project memory.
- `repo_search` tool for ranked indexed-file lookup with symbols, headings,
  imports, reasons, and short on-demand snippets.
- Smart local repo-map injection for repo/code prompts, disabled for cloud
  backends unless `projectMemory.allowCloudContext` is explicitly enabled.
- Deterministic extraction for Rust, Python, TypeScript/JavaScript, Markdown,
  JSON, TOML, and generic text files.

## [0.1.33] - 2026-04-28

### Added

- `/recommend [refresh] [all] [--cloud] [apply]` for hardware-aware model
  recommendations tuned for local coding-agent use.
- Safe hardware summary detection for OS, architecture, chip, machine name,
  memory, and CPU counts, cached under `.sessions/hardware.json` without raw
  serials, UUIDs, or UDIDs.
- Model candidate parsing for parameter size and quantization hints, memory-fit
  scoring, installed/default/cached candidate ranking, and explicit apply
  behavior for the active session.
- First-run setup now uses detected hardware memory to choose the default
  existing hardware profile.

## [0.1.31] - 2026-04-27

### Added

- Persistent capability cache under `.sessions/capabilities/` for per-backend
  and per-model probe results, tool-call support, inline JSON fallback support,
  usage chunk support, warnings, and benchmark stats.
- `/capabilities [refresh] [all]` to view the cached model scoreboard or refresh
  active/all backend probes.
- `/autotune [refresh] [all] [--cloud] [apply]` to score cached models, explain
  the best fit, and optionally apply the recommended backend/model/tool mode to
  the active session.
- `/doctor --deep` and `/bench` now feed the capability cache.

## [0.1.30] - 2026-04-26

### Added

- First-run setup wizard that writes `agent.config.json`, chooses backend,
  hardware profile, model override, approval policy, and tool-selection mode,
  probes the selected backend, and can be rerun with `/setup`.
- Documented pre-1.0 commit-count versioning, starting with `0.1.30` for the
  setup release commit.
- `llamacpp` backend support for `llama-server` OpenAI-compatible endpoints,
  including `LLAMACPP_BASE_URL`, optional `LLAMACPP_API_KEY`, backend switching,
  docs, and startup troubleshooting hints.
- `/doctor --deep [all]` capability probing for model listing, streaming chat,
  usage chunks, native tool calls, inline JSON fallback, llama.cpp `--jinja`
  warnings, and saved JSON/Markdown reports under `.sessions/doctor/`.
- Efficiency mode with adaptive tool-schema selection (`toolSelection:
  "auto"` or `/tools auto`), prompt-budget breakdowns in `/context`, prompt
  budget warnings, prompt-cache re-warm fingerprints, and compacted large tool
  outputs.
- `.env` and `.env.local` config loading with process environment variables
  remaining highest priority.
- Workspace path policy (`workspaceRoot`, `outsideWorkspace`) for file tools
  and shell execution.
- Diff-first approval previews for `file_edit`, `file_write`, and the new
  approval-gated `apply_patch` tool.
- Session commands: `/sessions`, `/resume`, and `/export`.
- Input history persisted under `.sessions/history.jsonl`, arrow-key recall,
  cursor movement, and Ctrl-J multi-line prompts.
- Ctrl-C cancellation for active model streams and shell commands.
- Context and operations commands: `/config`, `/context`, `/compact`,
  `/doctor`, `/bench`, and `/eval`.
- Custom profile model maps in `agent.config.json`.
- Unit coverage for dotenv parsing, session loading, history persistence,
  workspace policy, and patch application.

## [0.1.0] - 2026-04-25

First Rust release. Full port of the original TypeScript implementation with
feature parity and a few quality-of-life additions.

### Added

- Rust binary (`small-harness`) replacing the Node/TypeScript entry point
- `SseParser` struct for incremental parsing of OpenAI-compatible
  Server-Sent-Events streams (decoupled from the HTTP layer for testability)
- Hand-rolled `reqwest` + SSE chat-completions client (no SDK dependency)
- Trait-object tool registry with seven tools: `file_read`, `file_write`,
  `file_edit`, `glob`, `grep`, `list_dir`, `shell`
- Tool-call reassembly across streamed chunks
- Inline-JSON tool-call fallback for small models that emit tool calls as
  plain text content instead of populating `tool_calls`
- Approval gate with three policies (`always`, `never`, `dangerous-only`) and
  per-tool / per-call session caching
- All four backends: Ollama, LM Studio, MLX, OpenRouter
- Hardware profiles: `mac-mini-16gb`, `mac-studio-32gb`
- Slash commands: `/help`, `/new`, `/clear`, `/session`, `/backend`,
  `/profile`, `/model`, `/tools`, `/compare`
- Bordered + plain TUI input modes, three loader styles, four tool-display
  modes, ASCII banner
- JSONL append-only session persistence under `.sessions/`
- Pre-warm at startup to populate the prompt-eval cache
- Unit tests (47): SSE parser, tool-call detection regex, inline-JSON fallback,
  unified diff, base64, ignore filter, dangerous-command regex, async tool
  execute paths against `tempfile`-backed directories
- GitHub Actions CI: build + test on Ubuntu and macOS; clippy + fmt-check
  enforcement

### Removed

- TypeScript implementation (`src/*.ts`, `package.json`, `tsconfig.json`,
  `node_modules/`)
- Node and `tsx` runtime dependency

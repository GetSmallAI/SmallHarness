# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `llamacpp` backend support for `llama-server` OpenAI-compatible endpoints,
  including `LLAMACPP_BASE_URL`, optional `LLAMACPP_API_KEY`, backend switching,
  docs, and startup troubleshooting hints.
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

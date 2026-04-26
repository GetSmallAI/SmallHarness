# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

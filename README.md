<p align="center">
  <img src="docs/assets/SmallHarness-ReadMeImage.png" alt="Small Harness" width="100%">
</p>

<h1 align="center">Small Harness</h1>

<p align="center">
  <strong>A TUI agent harness for small LLMs running on your Mac</strong>
</p>

<p align="center">
  <a href="#quick-install">Quick Install</a> &middot;
  <a href="#getting-started">Getting Started</a> &middot;
  <a href="#features">Features</a> &middot;
  <a href="#backends">Backends</a> &middot;
  <a href="#tools">Tools</a> &middot;
  <a href="#slash-commands">Slash Commands</a> &middot;
  <a href="#configuration">Configuration</a> &middot;
  <a href="#development">Development</a>
</p>

<p align="center">
  <a href="https://github.com/GetSmallAI/SmallHarness/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/GetSmallAI/SmallHarness/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-1.75%2B-dea584">
  <img alt="Version" src="https://img.shields.io/badge/version-0.1.31-111827">
  <img alt="Backends" src="https://img.shields.io/badge/backends-Ollama%20%7C%20LM%20Studio%20%7C%20MLX%20%7C%20llama.cpp%20%7C%20OpenRouter-2563eb">
  <img alt="Apple Silicon" src="https://img.shields.io/badge/Apple%20Silicon-optimized-111827">
  <img alt="License MIT" src="https://img.shields.io/badge/license-MIT-111827">
</p>

## What Is Small Harness?

Small Harness is a terminal-based agent harness for running small open-weight
LLMs locally on consumer Macs. It points the same TUI at five different
inference backends: [Ollama](https://ollama.com), [LM Studio](https://lmstudio.ai),
MLX, [llama.cpp](https://github.com/ggml-org/llama.cpp), or
[OpenRouter](https://openrouter.ai) cloud. The harness gives the model a
focused set of filesystem and shell tools, and gates dangerous operations
behind an approval prompt.

It is built for developers who want to use a 7B–14B model as an interactive
coding assistant without depending on a cloud API. Hardware profiles for the
Mac mini (16 GB) and Mac Studio (32 GB) pick sensible default models per
backend so you can start running without picking weights out of a long list.

## Features

| Area | What you get |
| --- | --- |
| First-run setup | Interactive wizard writes `agent.config.json`, picks backend/profile/model, chooses approval/tool mode, and probes the backend |
| Local-first | OpenAI-compatible chat completions against Ollama, LM Studio, MLX, or llama.cpp, all selectable at runtime |
| Cloud comparison | One-key A/B against any OpenRouter model with `/compare` |
| Hardware profiles | `mac-mini-16gb` and `mac-studio-32gb` map to model defaults sized for the box |
| Capability cache | `/doctor --deep` and `/bench` persist per-backend/model capability and latency records under `.sessions/capabilities/` |
| Autotune | `/autotune` scores cached models and can switch the active session to the best local fit |
| Configurable tools | File read/write/edit, apply-patch, glob, grep, list-dir, shell — pick which to enable to control prompt-eval cost |
| Approval gates | Per-tool prompts with diff previews, allow-once / allow-this-session / always-allow caching |
| Robust parsing | Inline JSON-shaped tool-call detector for small models whose templates skip the `tool_calls` field |
| Pre-warm at startup | Sends a 1-token request with the full system prompt + tools so the cache is hot before your first prompt |
| Efficiency mode | Auto-selects tool schemas per prompt, shows prompt-budget breakdowns, and compacts large tool outputs |
| Streaming output | Tokens stream as they arrive, with a grouped tool-call display |
| Session persistence | JSONL append-only session logs with list, resume, and export commands |
| Slash commands | `/setup`, `/backend`, `/profile`, `/model`, `/tools`, `/compare`, `/session`, `/sessions`, `/resume`, `/export`, `/doctor`, `/bench`, `/capabilities`, `/autotune`, `/eval`, `/new`, `/help` |
| Bordered TUI | Clean terminal box input with persisted history, arrow recall, and Ctrl-J multi-line prompts |

## Quick Install

You will need Rust (stable, 1.75+) and one local-inference backend running.

```bash
git clone https://github.com/morganlinton/SmallHarness.git
cd SmallHarness
cp .env.example .env
cargo run --release
```

Build a standalone binary with `cargo build --release` — it lands at
`target/release/small-harness` (~5 MB).

By default Small Harness talks to Ollama at `http://localhost:11434/v1`. To
target LM Studio, MLX, or llama.cpp instead, set `BACKEND=lm-studio`,
`BACKEND=mlx`, or `BACKEND=llamacpp` before running, or use `/backend` once
the harness is running.

If `agent.config.json` does not exist, the first run opens a short setup
wizard that writes one for you and probes the selected backend. Set
`SMALL_HARNESS_NO_WIZARD=true` to skip the wizard and use env/defaults only.

## Getting Started

### 1. Install a backend

Pick one. Ollama is the fastest path on a fresh box:

```bash
brew install ollama
brew services start ollama
ollama pull qwen2.5-coder:7b
```

LM Studio (already installed), MLX, and llama.cpp are also supported. See
[Backends](#backends) for ports and setup notes.

### 2. Run the harness

```bash
cargo run --release
```

On a fresh checkout, the setup wizard asks for backend, hardware profile,
optional model override, approval policy, and adaptive/fixed tool mode, then
writes `agent.config.json`. After setup, you will see the banner, a backend
probe, and a "Warming up" spinner that populates the prompt-eval cache so the
first prompt isn't slow. When the input box opens, type a question:

```
> what files are in src/?
```

### 3. Switch backends, profiles, and models on the fly

```
/backend lm-studio        switch to LM Studio
/backend llamacpp         switch to llama.cpp
/setup                    rerun setup and rewrite agent.config.json
/profile mac-studio-32gb  switch the hardware profile (changes default model)
/model                    list models from the current backend and pick one
/tools                    show enabled tools and auto/fixed selection mode
/compare                  run the same prompt against OpenRouter cloud
/sessions                 list saved JSONL sessions
/resume latest            resume the newest saved session
/doctor                   check backend, config, rg, and session storage
/doctor --deep            probe stream, usage, and tool-call capabilities
/capabilities             show cached backend/model capability scores
/autotune                 recommend the best cached local model
```

### 4. Adjust the tool set for speed

Each tool definition costs prompt-eval time on small local models. Small
Harness defaults to `toolSelection: "auto"`, so ordinary chat sends no tool
schemas, file/code questions send read/search/list schemas, edit requests add
edit/patch schemas, and shell-ish prompts add `shell` when it is enabled.
The `tools` list is the allowed pool:

```
/tools auto                    adaptive tool selection (default)
/tools fixed                   always send every enabled tool schema
/tools file_read,grep,list_dir
/tools auto file_read,grep,list_dir
```

Or set persistently in `agent.config.json`:

```json
{ "tools": ["file_read", "file_edit", "grep", "list_dir"] }
```

## Backends

| Backend | Default URL | API style | Best for |
| --- | --- | --- | --- |
| `ollama` | `http://localhost:11434/v1` | OpenAI-compatible | Easiest setup; mature tool-call templates; CLI model management |
| `lm-studio` | `http://localhost:1234/v1` | OpenAI-compatible | GUI model browser; explicit load/unload controls |
| `mlx` | `http://localhost:8080/v1` | OpenAI-compatible (via `mlx_lm.server`) | Fastest inference on Apple Silicon |
| `llamacpp` | `http://localhost:8080/v1` | OpenAI-compatible (via `llama-server`) | Direct GGUF serving; fastest path if you already use llama.cpp |
| `openrouter` | `https://openrouter.ai/api/v1` | OpenAI-compatible | Cloud A/B comparison; access to larger frontier models |

Override URLs with `OLLAMA_BASE_URL`, `LM_STUDIO_BASE_URL`, `MLX_BASE_URL`,
or `LLAMACPP_BASE_URL`. `openrouter` requires `OPENROUTER_API_KEY`.
`llamacpp` uses `LLAMACPP_API_KEY` only if your `llama-server` enforces one.

### Why not OpenRouter's Responses API?

The official `@openrouter/agent` SDK speaks OpenRouter's newer `/responses`
endpoint. Small Harness uses a hand-rolled `reqwest` + SSE client pointed at
each backend's `baseURL` because `/v1/chat/completions` is the common shape
across the supported local servers and OpenRouter cloud, even when a backend
also exposes newer endpoints.

## Tools

| Tool | Default | Approval | What it does |
| --- | --- | --- | --- |
| `apply_patch` | off | yes | Validate and apply a unified diff with `git apply --check` |
| `file_read` | on | no* | Read a file (text or image base64) with optional offset/limit |
| `file_edit` | on | yes | Search-and-replace edits with unique-match validation, returns unified diff |
| `grep` | on | no | Regex search file contents (uses ripgrep) |
| `list_dir` | on | no* | List directory entries, alphabetical, capped at 500 |
| `file_write` | off | yes | Write/create a file (overwrites) |
| `glob` | off | no* | Find files by glob pattern |
| `shell` | off | yes | Run a shell command, output capped at 256 KB |

`*` Read-only tools prompt when `outsideWorkspace` is `prompt` and the request
targets a path outside `workspaceRoot`.

Toggle the active set per session with `/tools`, per shell with the
`AGENT_TOOLS` env var, or persistently in `agent.config.json`.

### Approval policies

| Policy | Behavior |
| --- | --- |
| `always` (default) | Every call to a mutating tool prompts you |
| `dangerous-only` | Only `shell` calls matching `rm`, `sudo`, `chmod`, `dd`, `mkfs`, etc. prompt; safer commands run silently |
| `never` | No prompts (use only when you trust the model) |

At each prompt you can choose `[y]es`, `[n]o`, `[a]lways for this tool`, or
`[s]ession-allow this exact call`. The session cache resets on `/new`.

## Slash Commands

| Command | Description |
| --- | --- |
| `/help` | List available commands |
| `/setup` | Run the setup wizard, write `agent.config.json`, probe the backend, and apply the new config |
| `/new` | Start a fresh conversation |
| `/clear` | Clear the screen |
| `/config` | Show resolved backend, model, workspace, history, display, and context config |
| `/session` | Show backend, model, approval policy, session path, message count, total tokens |
| `/sessions` | List saved sessions under `.sessions/` |
| `/resume latest\|<id>` | Resume a saved session |
| `/export current\|<id> [markdown\|json] [path]` | Export a session transcript |
| `/backend [name]` | Switch backend (`ollama`, `lm-studio`, `mlx`, `llamacpp`, `openrouter`) |
| `/profile [name]` | Switch hardware profile (`mac-mini-16gb`, `mac-studio-32gb`) |
| `/model [id]` | List models from the current backend and pick one, or set directly |
| `/tools [auto\|fixed\|list]` | Show enabled tools, switch adaptive mode, or set the enabled pool: `/tools auto file_read,grep,list_dir` |
| `/compare [model]` | Re-send the last user message to OpenRouter cloud for A/B |
| `/context [maxMessages=N maxBytes=N]` | Show prompt budget, active adaptive tools, byte/token estimate, and context limits |
| `/compact [keep]` | Summarize older turns into a compact continuation session |
| `/doctor` | Check backend reachability, model list, `rg`, config, and session storage |
| `/doctor --deep [all]` | Probe OpenAI-compatible streaming, usage chunks, native tool calls, and inline JSON fallback, then save JSON/Markdown reports under `.sessions/doctor/` |
| `/bench [model]` | Measure warmup, first-token, total latency, and output rate |
| `/capabilities [refresh] [all]` | Show cached per-model capability and benchmark records, or refresh the active/all backend probes |
| `/autotune [refresh] [all] [--cloud] [apply]` | Score cached models, recommend the best fit, and optionally apply it to the active session |
| `/eval [prompt-file] [models]` | Run saved prompts against one or more models with tools off/on |
| `exit` | Quit |

`/doctor --deep` checks the active backend. Add `all` to probe every configured
backend with short timeouts; unreachable backends show as failed rows in the
capability table.

`/doctor --deep` and `/bench` also update `.sessions/capabilities/`. Use
`/capabilities` to view the local scoreboard and `/autotune apply` to switch
the current session to the best cached local model. Add `--cloud` when you want
OpenRouter records to compete with local models.

## Hardware Profiles

The profile drives the default model per backend. You can always override
with `AGENT_MODEL` or `/model`.

| Profile | Default Ollama model | Default LM Studio model | Default MLX model | Default llama.cpp model |
| --- | --- | --- | --- | --- |
| `mac-mini-16gb` | `qwen2.5-coder:7b` | `qwen2.5-coder-7b-instruct` | `mlx-community/Qwen2.5-Coder-7B-Instruct-4bit` | `gpt-3.5-turbo` |
| `mac-studio-32gb` | `qwen2.5-coder:14b` | `qwen2.5-coder-14b-instruct` | `mlx-community/Qwen2.5-Coder-14B-Instruct-4bit` | `gpt-3.5-turbo` |

The OpenRouter cloud default for both profiles is
`qwen/qwen-2.5-coder-32b-instruct`. The llama.cpp default mirrors the
`llama-server` OpenAI-compatible examples; use `/model` or start
`llama-server` with `--alias` if you want the loaded GGUF to advertise a
specific model id.

## Warmup

llama.cpp and llama.cpp-derived engines cache the prompt-eval result for any
prefix they have already seen. At startup, Small Harness sends a tiny
chat-completions request with the full system prompt + tool definitions and
`max_tokens: 1`.
That populates the cache, so your first real prompt only has to evaluate
the new user tokens — typically dropping first-prompt latency from ~12 s to
~2 s on a 7B q4 model.

Disable with `WARMUP=false` if you want a faster startup at the cost of a
slow first prompt.

The cache becomes stale when you change `/backend`, `/model`, or `/tools`.
The next prompt after a switch will pay the prompt-eval cost again.

## Configuration

### Environment variables

```bash
# Backend selection: ollama (default), lm-studio, mlx, llamacpp, openrouter
BACKEND=ollama

# Hardware profile: mac-mini-16gb (default) or mac-studio-32gb
PROFILE=mac-mini-16gb

# Override the model for the chosen backend
AGENT_MODEL=qwen2.5-coder:14b

# Per-backend endpoint overrides
OLLAMA_BASE_URL=http://localhost:11434/v1
LM_STUDIO_BASE_URL=http://localhost:1234/v1
MLX_BASE_URL=http://localhost:8080/v1
LLAMACPP_BASE_URL=http://localhost:8080/v1

# Optional if llama-server was started with API-key enforcement
LLAMACPP_API_KEY=sk-no-key-required

# Required when BACKEND=openrouter or you want /compare
OPENROUTER_API_KEY=sk-or-...

# Approval policy: always (default) | never | dangerous-only
APPROVAL_POLICY=always

# Active tools, comma-separated. Default: file_read,file_edit,grep,list_dir
AGENT_TOOLS=file_read,file_edit,grep,list_dir

# Tool schema selection: auto (default) or fixed
AGENT_TOOL_SELECTION=auto

# Pre-warm the model at startup (default: on)
WARMUP=true

# Skip first-run setup and rely on env vars / built-in defaults
SMALL_HARNESS_NO_WIZARD=false

# Maximum agent steps per turn
AGENT_MAX_STEPS=20

# Workspace safety: prompt (default), deny, allow
WORKSPACE_ROOT=/path/to/project
OUTSIDE_WORKSPACE=prompt

# Context/history tuning
AGENT_CONTEXT_MAX_MESSAGES=40
AGENT_CONTEXT_MAX_BYTES=262144
AGENT_HISTORY=true
AGENT_HISTORY_MAX_ENTRIES=200
```

### `agent.config.json`

For project-level defaults, run `/setup` or drop a JSON file in the repo root.
Anything you put here can be overridden by env vars or slash commands at
runtime.

```json
{
  "backend": "ollama",
  "profile": "mac-mini-16gb",
  "approvalPolicy": "dangerous-only",
  "tools": ["file_read", "file_edit", "grep", "list_dir"],
  "toolSelection": "auto",
  "maxSteps": 20,
  "workspaceRoot": "/path/to/project",
  "outsideWorkspace": "prompt",
  "context": {
    "maxMessages": 40,
    "maxBytes": 262144
  },
  "history": {
    "enabled": true,
    "maxEntries": 200
  },
  "profiles": {
    "mac-studio-fast": {
      "ollama": "qwen2.5-coder:14b",
      "llamacpp": "gpt-3.5-turbo",
      "openrouter": "qwen/qwen-2.5-coder-32b-instruct"
    }
  },
  "display": {
    "toolDisplay": "grouped",
    "inputStyle": "bordered",
    "loaderStyle": "spinner",
    "loaderText": "Thinking",
    "showBanner": true
  }
}
```

### Resolution order

1. Slash command overrides at runtime
2. Process environment variables (`BACKEND`, `PROFILE`, `AGENT_MODEL`, `AGENT_TOOLS`, …)
3. `.env.local`, then `.env`
4. `agent.config.json` in the working directory
5. Built-in defaults

## Architecture

```text
                +-------------------------+
                |        main.rs          |
                |  banner / input loop /  |
                |  warmup / approval      |
                +------------+------------+
                             |
                             v
+--------------+    +-------------------------+    +-------------------+
|  config.rs   |--->|        agent.rs         |<-->|   tools/*.rs      |
|  dotenv+JSON |    |  chat/completions loop  |    |  serde-typed,     |
|  + profiles  |    |  streaming + tool calls |    |  approval-gated   |
+--------------+    +------------+------------+    +-------------------+
                                 |
                                 v
                +-------------------------+
                |     backends.rs         |
                |  Ollama / LM Studio /   |
                |  MLX / llama.cpp /      |
                |  OpenRouter             |
                +-------------------------+
                             |
                             v
                +-------------------------+
                |   session.rs            |
                |  JSONL sessions/export  |
                +-------------------------+
```

## Development

```bash
cargo check                # type-check without producing a binary
cargo run                  # debug build + run (faster compile, slower runtime)
cargo run --release        # optimized build + run
cargo build --release      # produce target/release/small-harness
```

Project layout:

```text
src/
  main.rs             entry — input loop, loader, approval wiring, warmup
  agent.rs            chat/completions runner with tool calls + streaming
  backends.rs         Ollama / LM Studio / MLX / llama.cpp / OpenRouter endpoints + defaults
  config.rs           dotenv + agent.config.json loader, workspace/context/history config
  capabilities.rs     persistent model capability cache, scoring, and autotune helpers
  approval.rs         y/n/always/session-allow prompt with diff previews
  session.rs          JSONL conversation log, listing, resume, export helpers
  warmup.rs           pre-warm the prompt-eval cache at startup
  commands.rs         slash commands for sessions, config, backends, evals, doctor, bench
  renderer.rs         grouped tool display
  loader.rs           spinner / gradient / minimal loaders
  banner.rs           ASCII banner + dynamic backend/profile/model line
  input.rs            bordered + plain readers with history and multi-line input
  openai.rs           wire types + SSE streaming for chat completions
  tools/              apply_patch, file_read, file_write, file_edit, glob_tool, grep, list_dir, shell
```

Quality expectations:

- `cargo check` must pass cleanly.
- Tools that mutate filesystem state implement `require_approval` on the
  `Tool` trait (returning `true`, or computing it from the args for
  dangerous shapes — see `shell.rs`).
- New backends should expose an OpenAI-compatible `/v1/chat/completions`
  endpoint and add a profile-default model map in `backends.rs`.

Versioning:

- Small Harness stays on the `0.1.x` line before a larger product milestone.
- The patch number tracks the total repo commit count for the release commit.
  This capability-cache release is `0.1.31`: 30 commits were already on
  `main`, and the release commit is expected to be commit 31.
- Release tags should use a leading `v`, for example `v0.1.31`.

## Troubleshooting

### `Backend not reachable: Connection error`

The harness probes the backend at startup. If you see this message, the
named backend is not listening on the expected port. Suggestions:

- **Ollama**: `brew services start ollama`, or run `ollama serve` in a
  separate terminal. Default port 11434.
- **LM Studio**: open the app, go to "Local Server", click Start. Default
  port 1234.
- **MLX**: start `mlx_lm.server --port 8080` against an MLX-format model.
- **llama.cpp**: start `llama-server -m /path/to/model.gguf --host 127.0.0.1 --port 8080`.
  Add `--jinja` when you want native OpenAI-style tool calls.
- **OpenRouter**: set `OPENROUTER_API_KEY` in `.env`.

For backend-specific capability problems, run `/doctor --deep`. It exercises
`/v1/models`, streaming chat completions, usage chunks, a harmless tool-call
schema, and Small Harness' inline JSON fallback detector. Reports are saved to
`.sessions/doctor/` for sharing or comparison.

### First prompt is slow even with warmup

If you change `/backend`, `/model`, `/tools`, or the hardware profile after
warmup, the cached prefix becomes stale and the next prompt re-evaluates
the new system prompt + tools. This is one-time per change.

### Model returns tool calls as text JSON

Some small-model templates emit tool calls as plain content
(e.g. `{"name":"shell","arguments":{...}}`) instead of populating the
`tool_calls` field. Small Harness detects this pattern and synthesizes a
real tool call. If a particular model still misbehaves, switching to
`llama3.1:8b` (which has well-tested tool-call templates) usually resolves
it.

### Model responds in another language unexpectedly

Some bilingual models (notably the qwen family) drift into Chinese on short
greetings. The system prompt now includes an explicit language directive,
but you can strengthen it further by editing `SYSTEM_PROMPT` in
`src/config.rs`.

### `cargo: command not found`

Install Rust via [rustup](https://rustup.rs): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`.

## License

Small Harness is released under the [MIT License](LICENSE).

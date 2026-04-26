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
  <img alt="TypeScript" src="https://img.shields.io/badge/TypeScript-5.x-3178c6">
  <img alt="Node 18+" src="https://img.shields.io/badge/Node-18%2B-339933">
  <img alt="Backends" src="https://img.shields.io/badge/backends-Ollama%20%7C%20LM%20Studio%20%7C%20MLX%20%7C%20OpenRouter-2563eb">
  <img alt="Apple Silicon" src="https://img.shields.io/badge/Apple%20Silicon-optimized-111827">
  <img alt="License MIT" src="https://img.shields.io/badge/license-MIT-111827">
</p>

## What Is Small Harness?

Small Harness is a terminal-based agent harness for running small open-weight
LLMs locally on consumer Macs. It points the same TUI at four different
inference backends — [Ollama](https://ollama.com), [LM Studio](https://lmstudio.ai),
MLX, or [OpenRouter](https://openrouter.ai) cloud — gives the model a focused
set of filesystem and shell tools, and gates dangerous operations behind an
approval prompt.

It is built for developers who want to use a 7B–14B model as an interactive
coding assistant without depending on a cloud API. Hardware profiles for the
Mac mini (16 GB) and Mac Studio (32 GB) pick sensible default models per
backend so you can start running without picking weights out of a long list.

## Features

| Area | What you get |
| --- | --- |
| Local-first | OpenAI-compatible chat completions against Ollama, LM Studio, or MLX, all selectable at runtime |
| Cloud comparison | One-key A/B against any OpenRouter model with `/compare` |
| Hardware profiles | `mac-mini-16gb` and `mac-studio-32gb` map to model defaults sized for the box |
| Configurable tools | File read/write/edit, glob, grep, list-dir, shell — pick which to enable to control prompt-eval cost |
| Approval gates | Per-tool prompts with allow-once / allow-this-session / always-allow caching |
| Robust parsing | Inline JSON-shaped tool-call detector for small models whose templates skip the `tool_calls` field |
| Pre-warm at startup | Sends a 1-token request with the full system prompt + tools so the cache is hot before your first prompt |
| Streaming output | Tokens stream as they arrive, with a grouped tool-call display |
| Session persistence | JSONL append-only session log under `.sessions/` per conversation |
| Slash commands | `/backend`, `/profile`, `/model`, `/tools`, `/compare`, `/session`, `/new`, `/help` |
| Bordered TUI | Clean terminal box input, no terminal-background detection required |

## Quick Install

You will need Node 18+ and one local-inference backend running.

```bash
git clone https://github.com/morganlinton/SmallHarness.git
cd SmallHarness
npm install
cp .env.example .env
npm start
```

By default Small Harness talks to Ollama at `http://localhost:11434/v1`. To
target LM Studio or MLX instead, set `BACKEND=lm-studio` or `BACKEND=mlx`
before running, or use `/backend` once the harness is running.

## Getting Started

### 1. Install a backend

Pick one. Ollama is the fastest path on a fresh box:

```bash
brew install ollama
brew services start ollama
ollama pull qwen2.5-coder:7b
```

LM Studio (already installed) and MLX are also supported. See
[Backends](#backends) for ports and setup notes.

### 2. Run the harness

```bash
npm start
```

You will see the banner, a backend probe, and a "Warming up" spinner that
populates the prompt-eval cache so the first prompt isn't slow. When the
input box opens, type a question:

```
> what files are in src/?
```

### 3. Switch backends, profiles, and models on the fly

```
/backend lm-studio        switch to LM Studio
/profile mac-studio-32gb  switch the hardware profile (changes default model)
/model                    list models from the current backend and pick one
/tools                    show enabled tools, set with /tools file_read,grep
/compare                  run the same prompt against OpenRouter cloud
```

### 4. Adjust the tool set for speed

Each tool definition costs ~100 tokens of prompt-eval per turn. On a 7B
quantized model that's roughly 2 seconds per tool you keep around. The
default set is intentionally slim — pare it further for chat, expand for
coding sessions:

```
/tools file_read,grep,list_dir
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
| `openrouter` | `https://openrouter.ai/api/v1` | OpenAI-compatible | Cloud A/B comparison; access to larger frontier models |

Override URLs with `OLLAMA_BASE_URL`, `LM_STUDIO_BASE_URL`, or `MLX_BASE_URL`.
`openrouter` requires `OPENROUTER_API_KEY`.

### Why not OpenRouter's Responses API?

The official `@openrouter/agent` SDK speaks OpenRouter's newer `/responses`
endpoint. Local backends only expose `/v1/chat/completions`. Small Harness
uses the `openai` SDK pointed at each backend's `baseURL` so a single client
shape works everywhere — local servers and OpenRouter cloud — at the cost of
not using the Responses API.

## Tools

| Tool | Default | Approval | What it does |
| --- | --- | --- | --- |
| `file_read` | on | no | Read a file (text or image base64) with optional offset/limit |
| `file_edit` | on | yes | Search-and-replace edits with unique-match validation, returns unified diff |
| `grep` | on | no | Regex search file contents (uses ripgrep) |
| `list_dir` | on | no | List directory entries, alphabetical, capped at 500 |
| `file_write` | off | yes | Write/create a file (overwrites) |
| `glob` | off | no | Find files by glob pattern |
| `shell` | off | yes | Run a shell command, output capped at 256 KB |

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
| `/new` | Start a fresh conversation (resets approval cache and session file) |
| `/clear` | Clear the screen |
| `/session` | Show backend, model, approval policy, session path, message count, total tokens |
| `/backend [name]` | Switch backend (`ollama`, `lm-studio`, `mlx`, `openrouter`) |
| `/profile [name]` | Switch hardware profile (`mac-mini-16gb`, `mac-studio-32gb`) |
| `/model [id]` | List models from the current backend and pick one, or set directly |
| `/tools [list]` | Show enabled tools or set them: `/tools file_read,grep,list_dir` |
| `/compare [model]` | Re-send the last user message to OpenRouter cloud for A/B |
| `exit` | Quit |

## Hardware Profiles

The profile drives the default model per backend. You can always override
with `AGENT_MODEL` or `/model`.

| Profile | Default Ollama model | Default LM Studio model | Default MLX model |
| --- | --- | --- | --- |
| `mac-mini-16gb` | `qwen2.5-coder:7b` | `qwen2.5-coder-7b-instruct` | `mlx-community/Qwen2.5-Coder-7B-Instruct-4bit` |
| `mac-studio-32gb` | `qwen2.5-coder:14b` | `qwen2.5-coder-14b-instruct` | `mlx-community/Qwen2.5-Coder-14B-Instruct-4bit` |

The OpenRouter cloud default for both profiles is
`qwen/qwen-2.5-coder-32b-instruct`.

## Warmup

llama.cpp (Ollama's engine) caches the prompt-eval result for any prefix it
has already seen. At startup, Small Harness sends a tiny chat-completions
request with the full system prompt + tool definitions and `max_tokens: 1`.
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
# Backend selection: ollama (default), lm-studio, mlx, openrouter
BACKEND=ollama

# Hardware profile: mac-mini-16gb (default) or mac-studio-32gb
PROFILE=mac-mini-16gb

# Override the model for the chosen backend
AGENT_MODEL=qwen2.5-coder:14b

# Per-backend endpoint overrides
OLLAMA_BASE_URL=http://localhost:11434/v1
LM_STUDIO_BASE_URL=http://localhost:1234/v1
MLX_BASE_URL=http://localhost:8080/v1

# Required when BACKEND=openrouter or you want /compare
OPENROUTER_API_KEY=sk-or-...

# Approval policy: always (default) | never | dangerous-only
APPROVAL_POLICY=always

# Active tools, comma-separated. Default: file_read,file_edit,grep,list_dir
AGENT_TOOLS=file_read,file_edit,grep,list_dir

# Pre-warm the model at startup (default: on)
WARMUP=true

# Maximum agent steps per turn
AGENT_MAX_STEPS=20
```

### `agent.config.json`

For project-level defaults, drop a JSON file in the repo root. Anything you
put here can be overridden by env vars or slash commands at runtime.

```json
{
  "backend": "ollama",
  "profile": "mac-mini-16gb",
  "approvalPolicy": "dangerous-only",
  "tools": ["file_read", "file_edit", "grep", "list_dir"],
  "maxSteps": 20,
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
2. Environment variables (`BACKEND`, `PROFILE`, `AGENT_MODEL`, `AGENT_TOOLS`, …)
3. `agent.config.json` in the working directory
4. Built-in defaults

## Architecture

```text
                +-------------------------+
                |        cli.ts           |
                |  banner / input loop /  |
                |  warmup / approval      |
                +------------+------------+
                             |
                             v
+--------------+    +-------------------------+    +-------------------+
|  config.ts   |--->|        agent.ts         |<-->|   tools/*.ts      |
|  env + JSON  |    |  chat/completions loop  |    |  zod-typed,       |
|  + profiles  |    |  streaming + tool calls |    |  approval-gated   |
+--------------+    +------------+------------+    +-------------------+
                                 |
                                 v
                +-------------------------+
                |     backends.ts         |
                |  Ollama / LM Studio /   |
                |  MLX / OpenRouter       |
                +-------------------------+
                             |
                             v
                +-------------------------+
                |   session.ts            |
                |  JSONL append-only log  |
                +-------------------------+
```

## Development

```bash
npm run typecheck          # tsc --noEmit
npm start                  # tsx src/cli.ts
npm run dev                # tsx watch src/cli.ts
```

Project layout:

```text
src/
  cli.ts              entry — input loop, loader, approval wiring, warmup
  agent.ts            chat/completions runner with tool calls + streaming
  backends.ts         Ollama / LM Studio / MLX / OpenRouter — endpoint + per-profile defaults
  config.ts           env + agent.config.json loader
  approval.ts         y/n/always/session-allow prompt
  session.ts          JSONL append-only conversation log
  warmup.ts           pre-warm the prompt-eval cache at startup
  commands.ts         /help /new /clear /session /backend /profile /model /tools /compare
  renderer.ts         grouped tool display
  loader.ts           spinner / gradient / minimal loaders
  banner.ts           ASCII banner + dynamic backend/profile/model line
  input-styles.ts     bordered + plain readers
  tools/              file_read, file_write, file_edit, glob, grep, list_dir, shell
```

Quality expectations:

- Type-check (`npm run typecheck`) must pass.
- Tools that mutate filesystem state require `requireApproval` in their
  definition (or a function returning `true` for dangerous arg shapes).
- New backends should expose an OpenAI-compatible `/v1/chat/completions`
  endpoint and add a profile-default model map in `backends.ts`.

## Troubleshooting

### `Backend not reachable: Connection error`

The harness probes the backend at startup. If you see this message, the
named backend is not listening on the expected port. Suggestions:

- **Ollama**: `brew services start ollama`, or run `ollama serve` in a
  separate terminal. Default port 11434.
- **LM Studio**: open the app, go to "Local Server", click Start. Default
  port 1234.
- **MLX**: start `mlx_lm.server --port 8080` against an MLX-format model.
- **OpenRouter**: set `OPENROUTER_API_KEY` in `.env`.

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
`src/config.ts`.

### `tsx: command not found`

Run `npm install` from the repo root to fetch the dev dependencies.

## License

Small Harness is released under the [MIT License](LICENSE).

<h1 align="center">Small Harness</h1>

<p align="center">
  <strong>A small, terminal-first coding harness. Bring your own model, your own key, or your own MCP server.</strong>
</p>

<p align="center">
  <a href="#install">Install</a> &middot;
  <a href="#run-it">Run it</a> &middot;
  <a href="#first-session">First session</a> &middot;
  <a href="#backends">Backends</a> &middot;
  <a href="#tools-and-commands">Tools &amp; commands</a> &middot;
  <a href="#cost-and-credentials">Cost &amp; credentials</a> &middot;
  <a href="#going-further">Going further</a> &middot;
  <a href="#configuration">Configuration</a> &middot;
  <a href="#troubleshooting">Troubleshooting</a>
</p>

<p align="center">
  <a href="https://github.com/GetSmallAI/SmallHarness/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/GetSmallAI/SmallHarness/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-1.75%2B-dea584">
  <img alt="Version" src="https://img.shields.io/badge/version-0.4.9-111827">
  <img alt="Backends" src="https://img.shields.io/badge/backends-Ollama%20%7C%20LM%20Studio%20%7C%20MLX%20%7C%20llama.cpp%20%7C%20OpenRouter%20%7C%20OpenAI-2563eb">
  <img alt="Apple Silicon" src="https://img.shields.io/badge/Apple%20Silicon-optimized-111827">
  <img alt="License MIT" src="https://img.shields.io/badge/license-MIT-111827">
</p>

---

## What it is

A coding agent that lives in your terminal. Bring your own **API key**
(OpenAI or OpenRouter) or point it at a **local model** (Ollama, LM Studio,
MLX, llama.cpp) — same tools, same commands, same session log either way. It
ships with the usual tool kit — read, edit, grep, shell, run tests — plus a
few that aren't usual:

- **Local or cloud, one TUI.** Switch providers mid-session with
  `/backend <name>` — the tools, commands, and session log don't change.
- **Per-turn cost on the status line.** `$0.003 this turn · $0.41 session`
  when you're on a cloud backend with a cataloged model. Local turns just
  show tokens.
- **Real undo.** `/undo` reverts the last agent turn's file mutations,
  including files the agent created or files that weren't tracked when the
  turn started.
- **Session paths.** `/path fork` branches the conversation and workspace so
  you can try two fixes, diff them, and `/path pick` the winner — no
  worktree required.
- **MCP-native.** Drop servers into `mcpServers` in your config; their tools
  show up as `mcp__<server>__<tool>` to the model on next launch.
- **`/auth` instead of `.env`.** Paste API keys once into a `0600` file
  under `~/.config/small-harness/`. Env vars still win when set.
- **Approval gates you can live with.** Every mutating call shows you the
  diff first, with `allow once / allow session / always allow` caching.

---

## Install

**Homebrew (macOS):**

```bash
brew install getsmallai/tap/small-harness
```

**From source** (Rust 1.75+):

```bash
git clone https://github.com/GetSmallAI/SmallHarness.git
cd SmallHarness
cargo build --release    # binary at target/release/small-harness
```

---

## Run it

Launch the interactive session:

```bash
small-harness
```

> From a source checkout without installing, use `cargo run --release` instead.

The first launch runs a short setup wizard (it writes `agent.config.json` —
backend, model, approval policy). Skip it with `SMALL_HARNESS_NO_WIZARD=true`.
Every launch after that opens straight into a session.

Small Harness talks to **one backend at a time** — pick the path that fits.

### Path A — Cloud API key

*Fastest to start, frontier-model quality, nothing to install locally.*

1. Set your key — OpenAI **or** OpenRouter:

   ```bash
   export OPENAI_API_KEY=sk-...
   # or
   export OPENROUTER_API_KEY=sk-or-...
   ```

2. Launch, then select the provider in the first-run wizard (or any time with
   `/backend openai`):

   ```bash
   small-harness
   ```

Prefer not to put the key in your environment? Launch first, then run
`/auth set openai` inside the app and paste it once — it's stored in a `0600`
file under `~/.config/small-harness/`. Cost per turn and per session shows
live on the status line.

### Path B — Local model

*Private, free, offline — runs entirely on your machine.*

1. Install [Ollama](https://ollama.com), start it, and pull a coding model:

   ```bash
   brew install ollama
   brew services start ollama
   ollama pull qwen2.5-coder:7b
   ```

2. Launch — Ollama is the default backend, so there's nothing else to set:

   ```bash
   small-harness
   ```

LM Studio, MLX, and llama.cpp work the same way — see [Backends](#backends)
for their ports and start commands.

> **Tip:** switch backends mid-session with `/backend <name>`, and run
> `/doctor` if a backend won't connect.

---

## First session

```text
> what files are in src/?

  Listed src/  (24 files)

src/ has 24 Rust files: main.rs is the entry point (input loop, banner,
warmup); agent.rs runs the chat-completions loop; backends.rs handles the
providers; tools/ contains the tool implementations…

  1.2k in · 87 out · $0.0003 this turn · $0.0003 session

> add a function in src/util.rs that lowercases a string and trims it

  Read src/util.rs
  Edited src/util.rs

  --- src/util.rs
  +++ src/util.rs
  @@ ...
  +pub fn normalize(input: &str) -> String {
  +    input.trim().to_lowercase()
  +}

  Apply? [y/n/a]: y
  checkpoint saved (1 file) — /undo to revert
  3.4k in · 412 out · $0.001 this turn · $0.0013 session
```

A handful of moves worth knowing right away:

- `/mode explore | edit | ship | review` toggles tool + approval + step-budget
  presets.
- `/undo` reverts the last turn's file mutations.
- `/path fork` branches the session to try an alternate approach; `/path switch`,
  `/path diff`, and `/path pick` compare and merge paths.
- `/shipcheck` summarizes git state; `/handoff` drafts a commit message,
  changelog bullets, and a release post from local context.
- `/play fix-failing-test` runs a bundled demo in an isolated sandbox so you
  can try a real agent loop without touching your repo.
- `Ctrl-J` for newline; `Enter` submits.
- `small-harness --continue` resumes the most recent session in the cwd.

---

## Backends

| Backend | Default URL | Notes |
|---------|-------------|-------|
| `ollama` | `http://localhost:11434/v1` | Easiest setup; mature tool-call templates |
| `lm-studio` | `http://localhost:1234/v1` | GUI model browser; explicit load / unload |
| `mlx` | `http://localhost:8080/v1` | Fastest inference on Apple Silicon (via `mlx_lm.server`) |
| `llamacpp` | `http://localhost:8080/v1` | Direct GGUF serving (via `llama-server`) |
| `openrouter` | `https://openrouter.ai/api/v1` | Cloud A/B with `/compare`; access to frontier models |
| `openai` | `https://api.openai.com/v1` | Direct provider access with your own key |

Switch at runtime with `/backend <name>`. Endpoint overrides:
`OLLAMA_BASE_URL`, `LM_STUDIO_BASE_URL`, `MLX_BASE_URL`, `LLAMACPP_BASE_URL`,
`OPENAI_BASE_URL`. Cloud backends require an API key (set via
[`/auth`](#cost-and-credentials) or env var).

### Default model per backend

Each backend has one sensible default; local backends default to a 7B coder
that runs on modest hardware. Override any time with `/model`, `AGENT_MODEL`,
or `modelOverride` in your config.

| Backend | Default model |
|---------|---------------|
| `ollama` | `qwen2.5-coder:7b` |
| `lm-studio` | `qwen2.5-coder-7b-instruct` |
| `mlx` | `mlx-community/Qwen2.5-Coder-7B-Instruct-4bit` |
| `llamacpp` | `gpt-3.5-turbo` |
| `openrouter` | `qwen/qwen-2.5-coder-32b-instruct` |
| `openai` | `gpt-4o-mini` |

### Recommend the right model for your box

All model-tuning lives under `/doctor`:

```
/doctor recommend       rank installed + default + cached models for your hardware
/doctor autotune apply  switch to the top-scoring cached local model
/doctor --deep          probe streaming, usage chunks, tool calls, fallbacks
/doctor bench           measure warmup, first-token, and total latency
/doctor models          show cached per-model capability + benchmark records
```

---

## Tools and commands

### Tools

| Class | Tools |
|-------|-------|
| Read | `file_read`, `grep`, `list_dir`, `glob`, `repo_search` |
| Mutate (approval-gated) | `file_write`, `file_edit`, `apply_patch`, `batch_edit`, `shell` |
| Workflow | `run_tests`, `ship_status`, `web_fetch`, `update_plan`, `task` |
| MCP | anything an MCP server exposes, surfaced as `mcp__<server>__<tool>` |

The default `toolSelection: "auto"` keeps the full working pool available for
any real request (so "build me a site" writes files instead of dumping code
into the chat) and sends no tools only for plain greetings. `fixed` always
sends the pool. Set the pool with `/tools file_read,grep,list_dir`, or
persistently in `agent.config.json`.

### Approval policies

| Policy | Behavior |
|--------|----------|
| `always` (default) | Every mutating call prompts you, with a diff preview |
| `dangerous-only` | Only `shell` calls matching `rm`, `sudo`, `chmod`, `dd`, `mkfs`, etc. prompt |
| `never` | No prompts — use only when you trust the model |

At each prompt: `[y]es`, `[n]o`, `[a]lways for this tool`, or `[s]ession-allow
this exact call`. The session cache resets on `/new`.

### Slash commands

**Session and config**
```
/help                  list commands
/new                   start a fresh conversation
/setup                 rerun the setup wizard
/config                show resolved configuration
/session [title <…>]   show / rename the current session
/sessions              list saved sessions
/resume latest|<id>    resume a saved session
/export current|<id>   export transcript to markdown or json
/undo                  revert the last agent turn's file mutations
/path                  fork, switch, diff, pick, or drop parallel session paths
/paths                 list saved session paths
```

**Operator modes and workflow**
```
/mode explore|edit|ship|review   switch operator preset
/shipcheck                       summarize git + test readiness
/handoff                         draft commit, changelog, release copy
/test discover|run|smart         discover or run tests
/fix                             fix-until-green loop
/batch / /refactor               coordinated multi-file edits
/play fix-failing-test           bundled demo in an isolated sandbox
```

**Backend, model, tools**
```
/backend <name>        switch backend
/model [id]            list / pick a model (shows context + cost when known)
/tools auto|fixed|<…>  show or set the active tool pool
/auth                  manage API keys (list, set, clear)
/image <path>          attach an image to the next user turn
/reasoning on|off      toggle the streaming reasoning panel
/compare [model]       re-send the last prompt against OpenRouter for A/B
```

**Memory, capabilities, context**
```
/index                 build / refresh project memory
/map [query]           print a repo map or focused hits
/remember <text>       save a durable project note
/forget <id|all>       remove notes
/context               show prompt budget, model limit, auto-guard status
/compact               summarize older turns (auto-runs at threshold)
/doctor [--deep]       probe backend, tools, streaming, capabilities
/doctor models         show cached per-model capability + benchmark records
/doctor autotune       pick the best cached local model (add `apply` to switch)
/doctor recommend      rank models for your hardware
/doctor bench          measure warmup + first-token + total latency
/checkpoints           toggle per-turn snapshots
```

Run `/help` in the harness for the full list with descriptions.

---

## Cost and credentials

### Credentials with `/auth`

Cloud backends authenticate with API keys. Paste them once and Small Harness
stores them at `~/.config/small-harness/auth.json` (mode `0600`). Environment
variables always win at lookup time, so CI and scripted users see no change
in behavior.

```text
/auth                    show what's configured (keys are masked)
/auth set openai         paste your OpenAI key, save to file + this session
/auth set openrouter     paste your OpenRouter key
/auth clear openai       remove from the file (env stays for this session)
```

### Per-turn and session cost

When you're on a cloud backend with a model in the catalog (currently OpenAI's
GPT-4o family, o-series, GPT-4, GPT-3.5), every turn prints its own cost
plus the running session total:

```text
  2.1k in · 845 out · $0.013 this turn · $0.094 session
```

Switch to Ollama mid-session and the line shows `$0.00 this turn` but keeps
the running total honest. OpenRouter and not-yet-cataloged OpenAI models
show `$?` for the turn and prefix the session total with `≥` to signal it's
a lower bound, not a fiction.

The `/model` picker shows the same data while you choose:

```text
   1) gpt-4o-mini            128k ctx · $0.15/$0.60 per Mtoken
   2) gpt-4o                 128k ctx · $2.50/$10.00 per Mtoken
   3) o1-mini                128k ctx · $3.00/$12.00 per Mtoken
```

---

## Going further

### Project-specific system prompt

Drop a markdown file at `.small-harness/prompt.md` in your repo and Small
Harness prepends it to the system prompt every turn. Use it for project
conventions ("snake_case everywhere", "ship via `make release`", "never
edit `vendor/`"). Auto-truncated at 8 KB.

### MCP servers

Add an `mcpServers` block to `agent.config.json`:

```json
{
  "mcpServers": {
    "fs": {
      "command": "/usr/local/bin/some-mcp-server",
      "args": ["--root", "/tmp"],
      "env": { "TOKEN": "abc" }
    }
  }
}
```

Small Harness spawns each server at startup, lists its tools, and exposes
them through the same approval-gated tool layer with names like
`mcp__fs__read_file`. JSON-RPC over stdio; no extra dependencies.

### Image input

`/image <path>` attaches an image to your next prompt. Small Harness encodes
it as a `data:image/...;base64,...` URL and sends it as a multi-part user
message. The catalog tracks which models accept images; you get a warning
if your current model isn't vision-capable.

### Web fetch

`web_fetch` (off by default, approval-gated) lets the agent pull a URL,
strip HTML to text, and read the result. Useful for docs and RFCs the model
needs to consult mid-task. Enable per session with
`/tools auto file_read,grep,list_dir,web_fetch` or persistently in your
config.

### Project memory

`/index` builds a safe local repo map at `.sessions/project-memory/`. It
stores metadata only — paths, language, symbols, headings, capped keyword
terms — never file bodies. It honors `.gitignore` and skips `.git`,
`.sessions`, `target`, `node_modules`, binaries, oversized files, and
common secret/env files. `/map` prints a compact view; `/remember <text>`
saves a durable project note.

### Compare clouds with `/compare`

`/compare` re-sends your last prompt against any OpenRouter model so you can
A/B a local response against a frontier one without leaving the session.
Requires `OPENROUTER_API_KEY`.

---

## Configuration

Resolution order (later overrides earlier):

1. Built-in defaults
2. `agent.config.json` in the working directory
3. `.env`, then `.env.local`
4. Process environment variables
5. Slash command overrides at runtime

### Environment variables (the useful ones)

```bash
BACKEND=ollama                                          # ollama|lm-studio|mlx|llamacpp|openrouter|openai
AGENT_MODEL=qwen2.5-coder:14b                           # overrides the backend default model

OPENAI_API_KEY=sk-...                                   # required for openai
OPENROUTER_API_KEY=sk-or-...                            # required for openrouter / /compare
OPENAI_BASE_URL=https://api.openai.com/v1               # point at a compatible proxy if needed

APPROVAL_POLICY=always                                  # always | dangerous-only | never
AGENT_TOOLS=file_read,grep,list_dir,file_edit,file_write,shell,update_plan,task
AGENT_TOOL_SELECTION=auto                               # auto | fixed

WARMUP=true                                             # pre-warm prompt cache at startup
SMALL_HARNESS_NO_WIZARD=false                           # skip first-run setup
SMALL_HARNESS_NO_UPDATE_CHECK=false                     # skip the GitHub release check
```

Full list with comments in [`.env.example`](.env.example).

### `agent.config.json`

For project-level defaults, run `/setup` or drop a JSON file at the repo
root. Common shape:

```json
{
  "backend": "ollama",
  "modelOverride": "qwen2.5-coder:14b",
  "approvalPolicy": "dangerous-only",
  "tools": ["file_read", "grep", "list_dir", "file_edit", "file_write", "shell", "update_plan", "task"],
  "toolSelection": "auto",
  "maxSteps": 20,
  "workspaceRoot": "/path/to/project",
  "outsideWorkspace": "prompt",
  "context": {
    "maxMessages": 40,
    "modelContextTokens": 8192,
    "autoCompact": true,
    "compactThreshold": 0.85,
    "reserveRatio": 0.25
  },
  "projectMemory": {
    "enabled": true,
    "autoInject": true,
    "allowCloudContext": false
  },
  "checkpoints": { "enabled": true, "maxTurns": 10 },
  "paths": {
    "enabled": true,
    "maxPaths": 5,
    "maxSnapshotBytes": 52428800,
    "maxFileBytes": 1048576
  },
  "mcpServers": {
    "fs": { "command": "/usr/local/bin/some-mcp-server", "args": [] }
  }
}
```

Anything in the config can be overridden by env or slash commands at
runtime.

---

## Quality of life

- **`small-harness --continue`** resumes the most recent session in cwd
  without picking from a list.
- **`small-harness completions bash|zsh|fish`** prints a completion script
  you can source.
- **`/reasoning on|off`** toggles the streaming reasoning panel — adds a
  dim "thinking…" block above the answer for o-series and similar models.
- **Update check.** Once a day, Small Harness checks GitHub for a newer
  release and shows a one-line notice in the banner if there is one.
  Background, cached, opt-out with `SMALL_HARNESS_NO_UPDATE_CHECK=true`.
- **Crash log.** If the harness panics, it writes a redacted log (API keys
  scrubbed) to `.sessions/crashes/<timestamp>.log` and prints the path so
  you have something to attach to an issue.
- **One-shot mode** — `small-harness --print "summarize this repo"` or
  `printf '…\n' | small-harness` for scripts and CI. Approval-gated tools
  are denied by default; pass `--allow-tools` to allow them.
- **Warmup.** Small Harness sends a 1-token request with the full system
  prompt + tools at startup so llama.cpp-derived engines have a hot
  prompt-eval cache before your first prompt. Disable with `WARMUP=false`.

---

## Troubleshooting

### `Backend not reachable: Connection error`

- **Ollama** — `brew services start ollama` or run `ollama serve`. Default port 11434.
- **LM Studio** — open the app, go to Local Server, click Start. Default port 1234.
- **MLX** — start `mlx_lm.server --port 8080` against an MLX-format model.
- **llama.cpp** — `llama-server -m /path/to/model.gguf --host 127.0.0.1 --port 8080 --jinja` (the `--jinja` flag enables native tool calls).
- **OpenRouter** — set `OPENROUTER_API_KEY` (or use `/auth set openrouter`).
- **OpenAI** — set `OPENAI_API_KEY` (or use `/auth set openai`). Use `OPENAI_BASE_URL` for a compatible proxy.

Run `/doctor --deep` for a fuller capability probe (streaming, usage chunks,
native tool calls, inline JSON fallback). Reports land under `.sessions/doctor/`.

### First prompt is slow even with warmup

The cache becomes stale when you change `/backend`, `/model`, or `/tools`.
The next prompt re-evaluates the new system prompt and tools. One-time per
change.

### Model returns tool calls as text JSON

Some small-model templates emit tool calls as plain content
(`{"name": "shell", "arguments": {…}}`) instead of populating the
`tool_calls` field. Small Harness detects and synthesizes a real tool call.
If a particular model still misbehaves, `llama3.1:8b` has well-tested
tool-call templates.

### Model responds in another language unexpectedly

Some bilingual models (notably qwen) drift into Chinese on short greetings.
The system prompt has an explicit language directive; if it's still
happening, strengthen it by editing `SYSTEM_PROMPT` in `src/config.rs`.

### `cargo: command not found`

Install Rust via [rustup](https://rustup.rs):
`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`.

---

## Architecture

```text
                +-------------------------+
                |        main.rs          |
                |  banner / input loop /  |
                |  warmup / approval      |
                +------------+------------+
                             |
                             v
+-----------+    +-------------------------+    +-------------------+
| config.rs |--->|        agent.rs         |<-->|    tools/*.rs     |
|  + auth/  |    |  chat/completions loop  |    | + mcp__ adapters  |
+-----------+    +-------------+-----------+    +-------------------+
                               |
                               v
                +-------------------------+
                |       backends.rs       |
                |  Ollama / LM Studio /   |
                |  MLX / llama.cpp /      |
                |  OpenRouter / OpenAI    |
                +-------------------------+
```

Source layout in [`src/`](src/) — `agent.rs` runs the loop, `backends.rs`
holds the backend providers, `tools/` holds tool implementations, `mcp.rs` is
the stdio MCP client, `catalog.rs` has the per-model context + pricing
table, `auth.rs` manages the credential file, `session.rs` writes the JSONL
log. `cargo doc --open` for module-level docs.

---

## Contributing

```bash
cargo check                # type-check without producing a binary
cargo run --release        # optimized build + run
cargo build --release      # target/release/small-harness
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Guidelines:

- Mutating tools implement `require_approval` on the `Tool` trait (return
  `true`, or compute from args — see `shell.rs`).
- New backends need an OpenAI-compatible `/v1/chat/completions` endpoint
  and a default model in `backends.rs`.
- Before opening a PR, run the full check suite: `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, and `cargo test`.

Release tags use a leading `v` (`v0.4.0`). The release workflow at
[`.github/workflows/release.yml`](.github/workflows/release.yml) builds
notarized macOS binaries when Apple Developer secrets are present.

---

## License

[MIT](LICENSE).

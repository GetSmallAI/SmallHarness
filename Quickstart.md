# Small Harness Quickstart

This guide is for the first 20 minutes with Small Harness. It focuses on the
top three things you can do immediately: understand a repo, make a safe edit,
and tune Small Harness to the best model available on your machine.

## Before You Start

You need Rust and at least one OpenAI-compatible local backend. Ollama is the
fastest first path:

```bash
brew install ollama
brew services start ollama
ollama pull qwen2.5-coder:7b
```

Then run Small Harness from the project you want to work in:

```bash
cargo run --release
```

On first run, the setup wizard creates `agent.config.json`. For the quickest
local setup, choose:

```text
backend: ollama
profile: mac-mini-16gb
model override: blank
approval policy: dangerous-only
tool mode: auto
```

## 1. Understand A Codebase Fast

Small Harness is most useful when you let it inspect files directly instead of
pasting code into chat. Start with a broad map, then ask narrower questions.

Try:

```text
Give me a concise map of this repo. Focus on the entry points, core modules,
and where configuration lives.
```

Then:

```text
Find the code path for slash commands and explain how a new command should be
added.
```

Useful commands:

```text
/config       show the active backend, model, tools, workspace, and history
/tools        show enabled tools and whether adaptive tool selection is on
/context      show prompt budget and active tool schemas
```

What to look for:

- Small Harness should use read/search/list tools only when needed.
- With `toolSelection: "auto"`, ordinary chat should avoid sending tool schemas.
- The answer should cite concrete files and functions, not just guess.

## 2. Make A Safe Local Edit

Small Harness can edit files, but the best workflow is to ask for a small,
reviewable change and let approvals show you exactly what will happen.

Try:

```text
Add a short comment above the function that dispatches slash commands explaining
that new commands should be registered in both COMMANDS and dispatch.
```

Then inspect what happened:

```bash
git diff
cargo test
```

Useful commands:

```text
/session      show current model, approval policy, session file, and tokens
/new          start a clean conversation
/export current markdown
```

Good habits:

- Ask for one focused change at a time.
- Prefer exact files, functions, or tests when you know them.
- Keep `approvalPolicy` at `dangerous-only` or `always` until you trust a model.
- Use `git diff` as the source of truth before committing.

## 3. Tune The Best Local Model

Different local models vary a lot. Small Harness can probe model capabilities,
cache the results, benchmark latency, and recommend the best cached fit.

Run:

```text
/doctor --deep
/bench
/capabilities
```

If you have multiple backends running, probe them all:

```text
/capabilities refresh all
```

Then ask for a recommendation:

```text
/autotune
```

Apply the recommendation to the current session:

```text
/autotune apply
```

What Small Harness is checking:

- model listing
- streaming responses
- usage chunks
- native tool calls
- inline JSON fallback for small models
- first-token latency
- estimated output tokens per second

By default, `/autotune` prefers local backends. To let OpenRouter compete with
local models, use:

```text
/autotune --cloud
```

## A Good First Session

Here is a simple sequence that exercises the whole product:

```text
/config
Give me a concise map of this repo.
/doctor --deep
/bench
/capabilities
/autotune
Find one small README improvement and propose the exact diff before editing.
```

After the edit:

```bash
git diff
cargo fmt --all -- --check
cargo test
```

## Where Things Are Saved

Small Harness keeps local state under `.sessions/`:

```text
.sessions/
  history.jsonl          input history
  *.jsonl                session transcripts
  doctor/                deep doctor JSON and Markdown reports
  evals/                 eval suite JSON and Markdown reports
  capabilities/          per-model capability and benchmark cache
```

That local cache is what powers `/capabilities` and `/autotune`.

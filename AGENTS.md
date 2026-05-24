# Agent instructions (SmallHarness)

## Plan / feature builds

Before marking a plan **done** or saying the work is ready to commit, follow [docs/INTEGRATION_MATRIX.md](docs/INTEGRATION_MATRIX.md):

1. Fill the **Current build** section (plan name, date, plan-specific P-rows)
2. Check applicable **G** (global) and **F** (feature) rows; add regression tests for each new wiring path
3. Run `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
4. End with the **Completion report** template from that doc

(Humans start plan builds by pasting [docs/PLAN_KICKOFF.txt](docs/PLAN_KICKOFF.txt) above the plan.)

Unit tests passing alone is **not** sufficient when the change touches config, session state, operator modes, tools, or filesystem paths.

See also: [.cursor/rules/plan-ship-gate.mdc](.cursor/rules/plan-ship-gate.mdc)

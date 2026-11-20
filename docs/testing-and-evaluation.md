# Testing and Evaluation

## Four layers

| Layer | Tests | Real model/service? |
|---|---|---|
| Core and property | Stepper transitions, schema, budget invariants | No |
| Runtime concurrency | Tool batches, cancellation, timeouts, Failed paths | No (fake tools) |
| Host conformance and fault | Durable journal, restart, approval, OutcomeUnknown | No (fault injection) |
| End-to-end and evaluation | Coding Agent, adapters, real providers | Optional credentials |

## Agreed seams

Tests observe public behaviour, not private helpers.

1. **`Agent` end-to-end** (`autra-runtime`): scripted `FakeModel` plus real
   tools; assert outcome, status, counters and `RunEvent`s.
2. **`AgentStepper` transitions** (`autra-harness`): fixed accepted-event
   sequences; assert phase, history, pending calls, Effect intents and
   disposition.
3. **`Tool` contract** (`autra-harness`): call built-in tools against a
   temporary workspace; assert content, truncation, path rejection and errors.
4. **`AgentState` JSON snapshot** (`autra-core`): one fixed state compared to a
   literal JSON value.
5. **Provider mapping** (`autra-openai`): pure request/response mapping plus a
   wiremock HTTP contract test.
6. **Approval** (`autra-runtime`): approved, rejected, missing-approver and
   mixed-batch cases, plus the argument-digest unit test.
7. **Session** (`autra-runtime`): history seeding into turn two, distinct
   session ids, distinct turn ids, cancelled-turn history and failed-turn
   isolation.
8. **Context and Skills** (`autra-harness`): context assembly, budget
   truncation with preserved items and an end-to-end `load_skill` call.
9. **Planning** (`autra-runtime`): the update_plan tool, rendered statuses and
   `Agent::plan()` after a Run.
10. **Subagents** (`autra-runtime`): a named child answer, unknown child and
    child failure as an error outcome.
11. **Tracing** (`autra-runtime`): the run-start event is captured with
    `tracing-test`.
12. **Evaluation** (`autra-eval`): pass, fail and run-error reports.
13. **Contract checks** (`autra-testkit`): built-in Tool, Model, Stepper and Host
    pass protocol checks.
14. **Observer hooks** (`autra-runtime`): a recording hook sees run, model,
    effect and finish stages.
15. **Effect ledger** (`autra-runtime`): model/tool records, committed, failed,
    rejected and outcome-unknown attempts.
16. **SQLite journal** (`autra-durable-sqlite`): the Run snapshot, accepted
    events and EffectRecords survive reopen through `AgentBuilder::journal`.
17. **Outbox** (`autra-durable-sqlite`): Authorized effects enqueue, claim with
    attempts, release and complete.
18. **Outbox dispatcher loop** (`autra-runtime`): a claimed tool or model effect
    runs through a Dispatcher, is recorded as committed, failed or
    outcome-unknown, and leaves the outbox; a cancelled claim is removed.
19. **Journaled Agent execution** (`autra-runtime`): a journaled Run enqueues
    and claims every effect, including bounded parallel tool batches; rejected
    effects never enter the outbox.
20. **Dispatcher** (`autra-runtime`): a custom dispatcher replaces model/tool
    execution and can run without a builder model.
21. **Workspace tools** (`autra-harness`, `autra-runtime`): `search_text` skips
    symlinks and build directories, caps results and stays inside the root;
    `edit_file` rejects missing, ambiguous and identical replacements;
    `write_file` creates parent directories; `run_command` runs in the
    workspace root, reports non-zero exits, and marks a timeout unknown;
    `git_status` and `git_diff` report repository state. An end-to-end Agent
    run writes a nested file and runs a command after two approvals.
22. **Approval preview** (`autra-harness`, `autra-runtime`): `write_file` and
    `edit_file` describe the pending diff, `run_command` shows the command, and
    the runtime carries the preview on `ApprovalRequest`.
23. **Resume** (`autra-runtime`, `autra-durable-sqlite`): an `AwaitingTool`
    checkpoint re-dispatches an `Authorized` effect and continues; a
    `Dispatched` call becomes `OutcomeUnknown`; an `AwaitingModel` checkpoint
    retires the stale model effect and re-issues it; completed runs and
    missing checkpoints are handled; a SQLite checkpoint survives reopen.
24. **Workspace CLI** (`autra-cli`): `run --workspace --read-only` uses the
    selected root, `--journal` persists a run, `resume` requires its journal
    and model, `tools` lists the workspace capabilities, and unknown options
    fail with usage.

CLI smoke tests cover `--help`, `tools`, the demo success/failure exit codes,
workspace flags and a missing provider key. Core `/implement` tests remain at
the Agent seam, not the CLI.

## Commands

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p autra-cli -- demo
```

No API key is required for the default suite.

## What is not covered yet

- Restart and crash recovery (no durable host).
- Durable approval waits, expiry and identity checks (the local flow has none).
- Parallel tool commit-order tests (parallel execution is not implemented).
- Real-provider quality and cost benchmarks.
- Long-running memory or context-compaction quality.

## Evaluation posture

Keep Agent quality (task success) separate from runtime correctness (no
duplicate effects, no orphan tasks, no false recovery). Any reliability or
security claim must ship with a reproducible counterexample test, per P9.

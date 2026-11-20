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
   session ids, cancelled-turn history and failed-turn isolation.
8. **Context and Skills** (`autra-harness`): context assembly, budget
   truncation with preserved items and an end-to-end `load_skill` call.

CLI smoke tests cover `--help`, `tools`, the demo success/failure exit codes and
a missing provider key. Core `/implement` tests remain at the Agent seam, not
the CLI.

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

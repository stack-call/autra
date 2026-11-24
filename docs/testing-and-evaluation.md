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
5. **Provider mapping** (`autra-openai`): pure request/response mapping plus
   wiremock HTTP and SSE contract tests.
6. **Approval** (`autra`): approved, rejected, missing-approver and
   mixed-batch cases, plus the argument-digest unit test.
7. **Session** (`autra`): history seeding into turn two, distinct
   session ids, distinct turn ids, cancelled-turn history and failed-turn
   isolation.
8. **Context and Skills** (`autra-harness`): context assembly, budget
   truncation with preserved items and an end-to-end `load_skill` call.
9. **Planning** (`autra-runtime`): the update_plan tool, rendered statuses and
   `Agent::plan_for(&RunId)` after a Run.
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
    `Dispatched` read-only or idempotent tool is retried while a
    non-idempotent one becomes `OutcomeUnknown`; an `AwaitingModel` checkpoint
    retires the stale model effect and re-issues it; completed runs and missing
    checkpoints are handled; a SQLite checkpoint survives reopen.
24. **Outbox reclaim** (`autra-runtime`, `autra-durable-sqlite`): an expired
    lease can be reclaimed; a terminal record is cleaned up without dispatch;
    an in-flight record becomes `OutcomeUnknown`; `run_due` drains rows across
    runs.
25. **Atomic transition commit** (`autra-durable-sqlite`): state, event,
    effects and Authorized outbox rows commit together; a stale
    `state_version` is rejected; a duplicate event sequence rolls the whole
    transaction back.
26. **Workspace CLI** (`autra-cli`): `run --workspace --read-only` uses the
    selected root, `--journal` persists a run, `resume` requires its journal
    and model, `outbox` requires its journal, `tools` lists the workspace
    capabilities, and unknown options fail with usage.
27. **Architecture** (`autra-testkit`): `cargo metadata` confirms core has no
    Autra dependencies, execution depends on core, harness and runtime depend
    only on core + execution, durable adapters do the same, and CLI/eval use
    the facade rather than runtime.
28. **ChildRun** (`autra-core`, `autra-runtime`, `autra`): a child has its own
    run id and parent link; parent cancellation reaches the child while child
    cancellation does not reach the parent; `WaitAll` collects every child and
    `FailFast` cancels and drains the rest; child Runs can share a journal and
    approver, forward events, and be resumed; a child gets its own budget capped
    by the parent's remaining tool calls without charging the parent.
29. **SessionStore** (`autra`, `autra-durable-sqlite`, `autra-cli`): turns are
    recorded as completed, cancelled or failed; history restores across
    process exits; saved turns upsert by index; `chat --session` requires a
    journal and persists turns across two CLI processes.
30. **Tool policy** (`autra-execution`, `autra-runtime`, `autra`): the baseline
    asks for `Unknown` or approval-required tools; a deny rule blocks without
    an approver; an allow rule cannot lower the baseline; deny commands block
    `run_command`; approval requests carry revision, actor and expiry; the CLI
    loads a JSON policy file.
31. **Streaming** (`autra`, `autra-openai`): deltas arrive before the final
    model result on both the direct and journaled paths; a non-streaming
    provider emits one delta; the OpenAI adapter parses SSE content and
    tool-call fragments.
32. **Snapshots** (`autra-runtime`, `autra-cli`): a hub records a final
    snapshot for new, resumed and failed Runs; subscriptions receive events and
    end at finish; an early model/tool error still closes the channel;
    subscribing after finish returns the snapshot; `autractl snapshot` reads a
    journaled Run.
33. **Approval persistence** (`autra-execution`, `autra-runtime`,
    `autra-durable-sqlite`, `autra-cli`): decisions are stored and reused on
    resume when the digest and policy revision match and the record is not
    expired; expired, mismatched or rejected records re-ask or reject; resume
    re-evaluates policy (Deny rejects, Allow authorizes); a decision from another
    policy revision is not reused; CLI `approve`/`reject` write decisions.
34. **Reconciliation** (`autra-runtime`, `autra-cli`): a `ReadOnly` unknown is
    retried; a non-idempotent unknown, a `Reconcileable` unknown, or an
    idempotent write without a valid approval stays unresolved; model retries
    can be disabled; a current policy `Deny` prevents retry; `effects --unknown`
    lists pending effects and `reconcile` reports retried and unresolved ids.
35. **TUI sessions** (`autra-cli`): restored user, assistant and tool history
    renders as chat lines; an empty session starts clean; each turn runs
    through `Session` so the turn index and stored history advance; `tui
    --session` without `--journal` fails before the terminal opens; `/resume`
    without a recorded Run explains its usage and `/session` reports the id,
    turn count and last Run. `Session::last_run_id` survives reopen through
    `SessionStore`.
36. **Local runtime P0** (`autra-runtime`, `autra-durable-sqlite`,
    `autra-harness`, `autra`): a long Effect renews its claim instead of being
    reclaimed; a stale claim cannot record an outcome; Run leases are exclusive,
    released, and skipped by recovery while live; the recovery daemon has a
    single service lease; resume refuses an owned Run or a different Agent
    profile; cancellation stops a command and its grandchildren; atomic writes
    reject a stale pre-image and symlinks and leave no temp files.
37. **ReAct state reuse** (`autra-harness`): transitions keep history behavior
    while reusing the decoded engine state; the existing ReAct, resume and
    journal tests continue to pass (ADR-0058).
38. **Global scheduler** (`autra-runtime`, `autra`): a pool blocks after its
    limit and releases on drop; a cancelled waiter stops; two concurrent Runs
    share one global tool limit; 100 concurrent Runs stay under the configured
    tool limit; process tools use the process pool.
39. **Recovery supervisor** (`autra`): only one supervisor can hold the service
    lease; dropping the lease allows another supervisor to claim it; the
    supervisor reuses `recover_once` instead of a second Run loop.
40. **Cancel and attempt identity** (`autra-execution`, `autra-durable-sqlite`,
    `autra`, `autra-cli`): a cancel request survives reopen, recovery cancels a
    Run with a persisted request, CLI `cancel` records a reason, and outbox
    attempts carry an `AttemptId`, worker id and fencing token.
41. **Workspace write lock** (`autra-harness`): a second file handle cannot take
    the workspace lock while the first is held; atomic writes release it on
    drop.
42. **Journal GC** (`autra-durable-sqlite`, `autra-cli`): only terminal Runs
    older than the cutoff are removed, dry-run reports without deleting, active
    Runs and their events/effects remain, and `gc` validates its journal flag.

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

- Automatic restart recovery; `resume` and `outbox` are explicit.
- Durable approval waits and verified actor identity. Persisted decisions,
  expiry metadata and resume reuse exist; identity is claimed, not verified.
- Parallel tool commit-order and crash-fault injection under a durable journal.
- Real-provider quality and cost benchmarks.
- Long-running memory or context-compaction quality.

## Evaluation posture

Keep Agent quality (task success) separate from runtime correctness (no
duplicate effects, no orphan tasks, no false recovery). Any reliability or
security claim must ship with a reproducible counterexample test, per P9.

# Roadmap

Status reflects the repository, not the ambition of the design proposal.
The first complete harness (§0.4) is reached at a basic level; see
[`harness.md`](harness.md).

## Milestones

| Milestone | Content | Status |
|---|---|---|
| M0 Architecture baseline | Spec, ADRs, workspace, fake model, minimal tool | Done |
| M1 Agent Core v0.1 | Stepper, ReAct, tool calling, local host, CLI, streaming events | Done |
| M2 Structured concurrency | Batches, budgets, cancellation, ChildRun, parallel tools | Tools, bounded parallelism and runtime ChildRun done, including parent budget reservation, shared journal/approver, event forwarding and FailFast drain |
| M3 Full interaction | Turns, approval, commands, hooks, tool policy | Approval, persisted/reusable approvals, guarded tools, policy registry, session/turn identity and SQLite `SessionStore` done; SessionManager pending |
| M4 Native durability | SQLite journal, outbox, checkpoints, recovery, OutcomeUnknown | Partial: Run snapshot, event log, EffectRecord, outbox v0; journaled Agent runs claim and dispatch through the outbox loop, and `resume` continues AwaitingTool/AwaitingModel checkpoints. Lease-safe reclaim, safe-class retry and atomic transition commit with `state_version` CAS done; automatic reconciliation pending |
| M5 Rich harness | Context policy, compaction, skills, planning, memory | Partial: ContextBuilder, budget, Skills and Planning done; memory pending |
| M6 Sandbox and evaluation | OpenShell adapter, tracing, replay, evaluation set | Tracing, model streaming (including SSE) and basic evaluation done; OpenShell and replay pending |
| M7 Restate PoC | Hosted execution, capability matrix, comparison | Pending |
| M8 Demand-driven expansion | MCP Gateway/Server, Python SDK, API server, remote workers, plugin manager | Pending (plugin design in [`plugins.md`](plugins.md)) |

### Dual-track view

Execution reliability and Agent experience advance in parallel; neither waits for
the other to finish.

| Stage | Execution and reliability | Agent experience |
|---|---|---|
| M0 | Small Core + Fake Host | Agent Builder + simple dialogue |
| M1 | Local Runtime + Tool/Effect | ReAct + streaming events + Session |
| M2 | Parallel tools, ChildRun, Scope | Context + Skills + coding tools |
| M3 | Approval + policy + workspace constraints | MCP Client + Planning + Subagents |
| M4 | Native Durable + recovery | Full coding-agent evaluation |
| M5 | OpenShell and Restate adapters | Advanced toolchains and reliable cases |
| Later | Remote Worker / Compute | Python SDK / more engines |

Every new runtime capability must be validated by a real Agent example. A basic
MCP Client is an M3 experience item; MCP Gateway/Server stays in M8.

## Delivered

- `autra-core`, `autra-harness`, `autra-runtime`, `autra-cli` MVP slices.
- Workspace agent tools with path confinement and bounded output: read, list, search, write and an approval-bound local command runner.
- Bounded parallel tool batches, opt-in and defaulting to sequential.
- Tool approval bound to Run, call, tool and argument digest; guarded write_file.
- In-memory Sessions with sequential turns, history seeding and Run session/turn identity; SQLite-backed `SessionStore` with turn records and restore.
- ContextBuilder seam with a character budget and on-demand Skills.
- Tool-backed in-memory planning via `update_plan` and `Agent::plan()`.
- Restricted subagents as named child Agents behind `spawn_subagent`.
- `tracing` spans for Sessions, Runs, model calls and tool calls.
- Basic outcome evaluation in `autra-eval`.
- In-memory EffectRecord/EffectAttempt ledger with stable EffectId and approval binding.
- Budgets, cooperative cancellation, optional Effect timeouts.
- Deterministic tests at the Agent, Stepper, Tool and snapshot seams.
- An OpenAI-compatible non-streaming provider crate.
- Extension seams: custom ContextBuilder/Stepper/Dispatcher, Tool descriptor/contract/context,
  ModelCapabilities, Host/Sandbox declarations, protocol checks and ObserverHook.
- Documentation and ADR baseline.

## Next slices

1. Automatic parent budget settlement for ChildRun and child event forwarding.
2. SessionManager, TurnRecord and a SessionStore boundary.
3. Verified actor identity and a richer policy surface.
4. Automatic `OutcomeUnknown` reconciliation for the outbox claim loop.
5. Memory retrieval, a network snapshot/subscribe protocol and a richer context policy.
6. OpenShell adapter and replay fixtures.

The ordering follows the spec's dependency logic: stabilize state and
concurrency semantics before adding durability, and add durability before
hosted execution.

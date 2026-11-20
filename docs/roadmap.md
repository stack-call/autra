# Roadmap

Status reflects the repository, not the ambition of the design proposal.
The first complete harness (§0.4) is reached at a basic level; see
[`harness.md`](harness.md).

## Milestones

| Milestone | Content | Status |
|---|---|---|
| M0 Architecture baseline | Spec, ADRs, workspace, fake model, minimal tool | Done |
| M1 Agent Core v0.1 | Stepper, ReAct, tool calling, local host, CLI, streaming events | Done |
| M2 Structured concurrency | Batches, budgets, cancellation, ChildRun, parallel tools | Tools, bounded parallelism and tool-backed subagents done; runtime ChildRun pending |
| M3 Full interaction | Turns, approval, commands, hooks, tool policy | Approval, guarded write_file and in-memory sessions with session and turn identity done; SessionManager and SessionStore pending |
| M4 Native durability | SQLite journal, outbox, checkpoints, recovery, OutcomeUnknown | Pending: a local `outcome_unknown` marker exists, no reconciliation |
| M5 Rich harness | Context policy, compaction, skills, planning, memory | Partial: ContextBuilder, budget, Skills and Planning done; memory pending |
| M6 Sandbox and evaluation | OpenShell adapter, tracing, replay, evaluation set | Tracing and basic evaluation done; OpenShell and replay pending |
| M7 Restate PoC | Hosted execution, capability matrix, comparison | Pending |
| M8 Demand-driven expansion | MCP, Python SDK, API server, remote workers | Pending |

## Delivered

- `autra-core`, `autra-harness`, `autra-runtime`, `autra-cli` MVP slices.
- Read-only workspace tools with path confinement and bounded output.
- Bounded parallel tool batches, opt-in and defaulting to sequential.
- Tool approval bound to Run, call, tool and argument digest; guarded write_file.
- In-memory Sessions with sequential turns, history seeding and Run session/turn identity.
- ContextBuilder seam with a character budget and on-demand Skills.
- Tool-backed in-memory planning via `update_plan` and `Agent::plan()`.
- Restricted subagents as named child Agents behind `spawn_subagent`.
- `tracing` spans for Sessions, Runs, model calls and tool calls.
- Basic outcome evaluation in `autra-eval`.
- Budgets, cooperative cancellation, optional Effect timeouts.
- Deterministic tests at the Agent, Stepper, Tool and snapshot seams.
- An OpenAI-compatible non-streaming provider crate.
- Documentation and ADR baseline.

## Next slices

1. Runtime ChildRun identity, cancellation and budget slicing.
2. SessionManager, TurnRecord and a SessionStore boundary.
3. Policy registry, hooks and an interactive approval surface.
4. Native durable SQLite host with a transactional outbox.
5. Memory retrieval and a richer context policy.
6. OpenShell adapter and replay fixtures.

The ordering follows the spec's dependency logic: stabilize state and
concurrency semantics before adding durability, and add durability before
hosted execution.

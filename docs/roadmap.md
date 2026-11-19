# Roadmap

Status reflects the repository, not the ambition of the design proposal.

## Milestones

| Milestone | Content | Status |
|---|---|---|
| M0 Architecture baseline | Spec, ADRs, workspace, fake model, minimal tool | Done |
| M1 Agent Core v0.1 | Stepper, ReAct, tool calling, local host, CLI, streaming events | Done |
| M2 Structured concurrency | Batches, budgets, cancellation, ChildRun, parallel tools | Tools: sequential and bounded parallel batches; budgets and cancellation done; ChildRun pending |
| M3 Full interaction | Turns, approval, commands, hooks, tool policy | Partial: commands/events; approval and sessions pending |
| M4 Native durability | SQLite journal, outbox, checkpoints, recovery, OutcomeUnknown | Pending: a local `outcome_unknown` marker exists, no reconciliation |
| M5 Rich harness | Context policy, compaction, skills, planning, memory | Pending |
| M6 Sandbox and evaluation | OpenShell adapter, tracing, replay, evaluation set | Pending |
| M7 Restate PoC | Hosted execution, capability matrix, comparison | Pending |
| M8 Demand-driven expansion | MCP, Python SDK, API server, remote workers | Pending |

## Delivered

- `autra-core`, `autra-harness`, `autra-runtime`, `autra-cli` MVP slices.
- Read-only workspace tools with path confinement and bounded output.
- Bounded parallel tool batches, opt-in and defaulting to sequential.
- Budgets, cooperative cancellation, optional Effect timeouts.
- Deterministic tests at the Agent, Stepper, Tool and snapshot seams.
- An OpenAI-compatible non-streaming provider crate.
- Documentation and ADR baseline.

## Next slices

1. Approval bound to tool, arguments, actor and policy revision.
2. Sessions and multiple turns.
3. Native durable SQLite host with a transactional outbox.
4. Context compaction, skills and memory.

The ordering follows the spec's dependency logic: stabilize state and
concurrency semantics before adding durability, and add durability before
hosted execution.

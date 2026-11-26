# Environments and a Durable Result Inbox Separate Control from Execution

**Status:** implemented on `refactor/runtime-environment-inbox`, pending review

## Context

Tools directly coupled file/process operations to LocalWorkspace. The executor
borrowed LocalEngine, and optional pooled execution delivered authoritative
outcomes through an in-memory channel. Existing Outbox, leases and Journal
transactions were a useful foundation, but could not preserve a complete
model result after Coordinator loss or atomically acknowledge its acceptance.

## Decision

Keep the Core Sans-I/O and preserve AgentStepper, Harness behaviour,
Dispatcher/ToolRegistry and TransitionCommitter. Introduce ExecutionEnvironment
and EnvironmentProvider contracts in autra-execution. Local and Sandbox
adapters implement these contracts; pure Model Effects need no environment.

Give EffectExecutor owned explicit dependencies. In optional pooled execution,
an independent worker publishes to a Journal Result Inbox. Notifications only
wake readers or carry non-authoritative model deltas. RunCoordinator alone
commits authoritative state transitions.

Make outcome publication atomic with Effect/Attempt updates and Outbox
retirement. Make result consumption atomic with versioned state, accepted
event, new Effects and Outbox publication. Both MemoryJournal and SqliteJournal
implement the boundary, with live Run ownership and Effect claims checked
inside the corresponding transaction/lock. Deduplicate by Effect/Attempt
identity, retain consumed tombstones and reject conflicting publications.

Persist environment requirements and result-delivery mode on EffectRecord.
Reserve scheduler resource/environment capacity before targeted claim; pin the
selected environment before external I/O. Enforce live lease expiry, fence
stale callbacks and retain claim generations across explicit reconciliation.

This extends ADR-0025 resource capability ownership and ADR-0040's transaction
boundary. It preserves ADR-0039: taking over an interrupted dispatched Effect
never automatically repeats its external action. It does not implement the
approval-parking proposal in ADR-0060.

## Consequences

- File/process tools can execute through Local or supplied remote environments
  without reaching for local resources as a fallback.
- Model output survives Coordinator loss, including tool calls and usage.
- Inline execution and old LocalWorkspace constructors remain available.
- SQLite version 2 migrates to 3. Custom Journal adapters must implement atomic
  Inbox/ownership methods before enabling pooled execution.
- Legacy resource-dependent Effects require an explicit environment binding.
- Quotas remain process-local; independent tasks are not a distributed worker
  service. External exactly-once execution is not promised.

Implementation, migration details and validation are documented in
[Runtime refactor](../runtime-refactor.md).

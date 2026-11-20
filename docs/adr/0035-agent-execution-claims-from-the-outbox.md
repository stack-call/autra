# Agent Execution Claims From the Outbox

**Status:** accepted

`OutboxDispatcher::run_once_for_run` existed, but the Agent never used it. The
coordinator called the Dispatcher directly, recorded outcome status itself and
only wrote the outbox when someone enqueued effects by hand. A journaled run
therefore executed effects outside the claim protocol the durable host depends
on.

When `AgentBuilder::journal` is set, every effect that reaches `Authorized` is
now recorded, enqueued and executed through the claim loop:

- a model call is claimed one at a time with `run_once_for_run`;
- a tool batch is claimed in one pass with `run_once_for_run_with`, bounded by
  `max_parallel_tool_calls`, and each finished tool is reported as it completes;
- an approval rejection never enqueues, a cancellation observed before an
  effect starts marks it `CancelledBeforeDispatch` and removes its outbox row;
- after a pass, the Agent reloads the affected records from the journal, so the
  Run's effect ledger is the journal's ledger rather than a parallel copy.

An Agent without a journal keeps the `LocalEphemeral` direct path, because
there is no outbox to claim from. `Dispatcher::knows_tool` (default `true`)
lets the loop keep the Agent's distinction between an unknown tool (`Failed`)
and a tool that returned an error (`Committed` with a failed result).

Consequences: journaled runs have one execution path, and the outbox no longer
accumulates rows the Agent never touches. Tool call start events for a claimed
batch are emitted only once dispatch begins, so a cancelled claim never leaves
a start event without a result. The loop still has no lease-expiry
retry, resume after a crash, or reconciliation of `OutcomeUnknown`; a model
pass claims one due row from the Run, so a stale row would run first. Effects
are claimed per turn, not as one durable transaction with the accepted event.

Alternatives considered: leaving the Agent direct and letting a background
worker drain the outbox (two writers, no shared ordering), and forcing tool
batches through the sequential loop (silently dropping bounded parallelism).

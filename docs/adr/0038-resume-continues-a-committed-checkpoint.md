# Resume Continues a Committed Checkpoint

**Status:** accepted

The SQLite journal persisted Run state, events and effects, but nothing could
use those records to continue a run. After a process exit, the only option was
to start over, which duplicated every effect the journal had already accepted.

`Agent::resume_with_options` loads the Run checkpoint and continues it:

- a terminal run returns its stored outcome without calling model or tools;
- an `AwaitingTool` checkpoint re-dispatches `Authorized` effects through the
  outbox, rejects calls whose approval was never completed, records an
  in-flight `Dispatched` call as `OutcomeUnknown` instead of retrying it, marks
  a missing effect record `Failed`, and feeds the reconstructed outcomes back
  as `ToolResult` events;
- an `AwaitingModel` checkpoint retires non-terminal model effects as
  `OutcomeUnknown` and emits `EventKind::ResumeRequested`, which the ReAct
  stepper translates into a fresh model call without consuming a turn.

The `Agent` worker loop was extracted into a private `drive` method so normal
runs and resumed runs share one coordinator. The CLI exposes `--journal` on
`run` and `chat`, and `autractl resume`, which requires an explicit `--model`
because provider configuration is not Run state.

Consequences: a checkpoint can be continued without replaying accepted events.
Safety follows the effect rules already in the journal: an in-flight
non-idempotent tool is never silently retried, while a model call is
re-issued. Approval is not durable, so a call interrupted before approval is
rejected on resume. Resume assumes the same Agent configuration, tools and
workspace; changing them changes what the run means. There is still no lease
expiry retry, no reconciliation for `OutcomeUnknown`, and no `SessionStore`.

Alternatives considered: replaying every accepted event (the state is already
committed, so replay would duplicate effects), retrying `Dispatched` tools
(violates the unknown-outcome rule), and storing provider configuration in the
journal (configuration is not execution state).

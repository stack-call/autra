# A Timed-Out Tool Is Outcome Unknown

**Status:** accepted

`tokio::time::timeout` drops the tool future. The external process or API may
still be running, or may already have succeeded with its result discarded.
Recording that as a definite failure would violate INV-03 and INV-06.

We decided a timed-out tool produces an `is_error` observation for the model
with `outcome_unknown = true`. The text states that the tool may still have run.
The model can react; the ledger does not claim a definite failure.

Consequences: `ToolOutcome` and `RunEvent::ToolResult` carry the flag; the CLI
displays `unknown`. The durable reconciliation pass does not exist yet, so the
flag is an honest marker, not a recovery mechanism.

Revisit when a durable host adds Effect reconciliation; the flag must then feed
that protocol rather than remain a display detail.

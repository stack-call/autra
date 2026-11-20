# A Reclaimed Lease Never Re-runs an In-flight Effect

**Status:** accepted

Outbox claims had a lease deadline, but the dispatcher had no reclaim path and
no rule for what a reclaimed row means. A crash could leave a row claimed
forever, and naively re-dispatching it would run a non-idempotent tool twice.

Claiming now distinguishes the effect record's state before dispatch:

- a terminal record (`Committed`, `Failed`, `Rejected`, `OutcomeUnknown`,
  `CancelledBeforeDispatch`) means the outcome was recorded but the row was not
  deleted; the row is completed without executing anything;
- a `Dispatched` record means an earlier attempt was in flight when its lease
  ended; it becomes `OutcomeUnknown` with an explicit attempt, and the row is
  completed without a second call;
- any other record is dispatched normally.

`OutboxDispatcher::run_due` claims due rows across Runs and applies those rules.
`Agent::drain_outbox` and `autractl outbox` expose it. Resume applies the same
principle earlier: an interrupted tool is retried only when its tool class is
`ReadOnly` or `Idempotent`; a non-idempotent tool becomes `OutcomeUnknown`.

Consequences: a reclaimed row cannot silently duplicate a side effect, and the
crash window between recording an outcome and deleting its outbox row is
self-healing. Retrying an `OutcomeUnknown` effect still needs an explicit human
decision or a future reconciliation policy; the ledger records what happened
but does not invent a result. Leases remain fixed at 30 seconds; there is no
background worker that drains due rows on a schedule.

Alternatives considered: re-dispatching every reclaimed row (unsafe for
non-idempotent effects), deleting reclaimed in-flight rows without a record
(loses the unknown state), and adding tool-level reconciliation callbacks
(premature before a policy registry exists).

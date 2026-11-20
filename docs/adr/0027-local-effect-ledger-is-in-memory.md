# Local Effect Ledger Is In-Memory

**Status:** accepted

Without an effect record, a Run could not explain which intents were accepted,
which were dispatched, and which result was committed; a recovery story would
have nothing to replay.

We added an in-memory `EffectRecord`/`EffectAttempt` ledger to the local
coordinator. The coordinator assigns a stable `EffectId` at acceptance, records
the intent digest and status lifecycle, and records one attempt per dispatch.
`RunOutcome::effects()` exposes the ledger, and approvals bind to the
`effect_id`.

Consequences: local Runs are explainable and cancellations can mark
`CancelledBeforeDispatch`; the ledger is lost on process exit. Durable
reconciliation, retry attempts and stable IDs across restarts remain future
work.

Alternatives considered: only logging outcomes (no identity), putting the ledger
in Core state (misses host ownership), and implementing SQLite now (premature).
Revisit with the durable host.

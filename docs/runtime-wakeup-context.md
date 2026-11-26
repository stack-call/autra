# Event-driven waits and registered Run execution context

Baseline: `2e0fbb86d596c205e1dc06e24ea486ac3d2eb437` (the measured runtime baseline).
The indexed implementation from `refactor/runtime-wakeup-context` is now on `mvp-v1`.
See the [scalability results](performance/runtime-scalability.md) for measurement status.

This stage implements local publication notifications, low-frequency recovery
checks, and an in-process RunContextRegistry. It retains one executor per Run;
shared cross-Run workers, durable Run Parking, remote transport and Sandbox
reattachment remain subsequent stages.

Outbox transitions and authorization notify the matching Run only after a
successful journal commit. Executor publication notifies Coordinator for every
completed result, without waiting for all Effects in a batch. Streams are
non-authoritative and do not require an Inbox read per delta. A shared Engine
clock wakes registered readers once per second to check Journal after a lost
notification, external publication or expired claim. These are per-Run checks
on a shared low-frequency tick, not a new global queue or distributed service.

RunExecutionContext holds owned executor dependencies, Run identity, cancellation,
a shared BudgetHandle, timeout, per-batch concurrency, child execution context
and Run-specific event routing. Registration is resolved by RunId; live duplicate
registrations are rejected, while a detached generation can drain committed
work without removing a replacement generation. Environment requirements remain
on EffectRecord and authorization remains in Coordinator.

Cancellation uses standard-library futures and wakers in Core, with parent-to-
child propagation and dropped-waiter cleanup. Core gains no Tokio or I/O dependency.
Journal claims, lease expiry, fencing, atomic result publication/consumption and
OutcomeUnknown semantics are retained.

All 387 workspace regression tests, 57 Harness tests without default features,
formatting and Clippy checks pass. Tests cover notification loss, publication
races, partial-batch result delivery, cancellation propagation/cleanup, context
generation replacement, shared budget reservations and clock restart across
Tokio runtimes. Repeat scalability measurements are pending.

The active Run's drive scope owns its ContextRegistration. The per-Run
worker holds only a shared execution-context handle. A worker exit leaves
the registration resolvable while the Run owns it. If the Coordinator
detaches, an in-flight worker can still publish committed work and keep
budget reservations available to a replacement. The registry recovery tick
prunes expired weak entries. Cross-process
workers still require trusted profile restoration, atomic durable budget
reservation and per-Run admission; a global Outbox batch must not reuse one
Run's DispatchOptions for other Runs.

## Indexed Outbox lookup and generation handover

The wakeup-only trial is retained in
`docs/performance/2026-11-26-wakeup.jsonl`. It reduced idle Journal traffic, but
showed a dense-work throughput regression and five failed stress cases. It is
an intermediate measurement, not the final implementation's performance claim.

Run-scoped Memory Outbox reads now use a secondary Run index, maintained with
Outbox publication/retirement under the same lock. SQLite uses a Run lookup
index and separate scoped/global queries. Existing schema-v3 databases acquire
the additive index on open; the data format and lease protocol are unchanged.
Tests verify index ordering, claim/release/retirement/reauthorization and SQLite
query-plan index selection after reopening a database without that index.

Within one Registry, a detached generation and its replacement share the same
budget handle, including prior reservations. A replacement cannot change
budget limits. Independent Registries/Engines/processes do not gain a shared
durable budget reservation protocol from this change.

The indexed implementation passes 387 workspace tests, formatting and Clippy.
Its full repeated performance matrix has not yet completed.

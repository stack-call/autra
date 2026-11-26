# Event-driven waits and registered Run execution context

Baseline: `2e0fbb86d596c205e1dc06e24ea486ac3d2eb437` (the measured runtime baseline).
Implementation and regression checks complete; performance rerun pending on `refactor/runtime-wakeup-context`.

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

All 385 workspace regression tests, 57 Harness tests without default features,
formatting and Clippy checks pass. Tests cover notification loss, publication
races, partial-batch result delivery, cancellation propagation/cleanup, context
generation replacement, shared budget reservations and clock restart across
Tokio runtimes. Repeat scalability measurements are pending.

The context registry is local and uses weak references with generation-aware
registration cleanup. It is not a persistent capability catalog. Cross-process
workers still require trusted profile restoration, atomic durable budget
reservation and per-Run admission; a global Outbox batch must not reuse one
Run's DispatchOptions for other Runs.

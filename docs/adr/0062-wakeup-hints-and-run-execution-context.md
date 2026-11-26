# Durable Readers Use Wake Hints and Resolve Owned Run Context

**Status:** implemented on `refactor/runtime-wakeup-context`, pending review

## Context

The measured runtime wakes each idle executor every 20ms. Coordinator waits and
scheduler cancellation waits also poll. Pooled execution at 10,000 waiting Runs
produced approximately 912,200 Journal API calls/s in Memory. A shared executor
pool alone would not preserve each Run's cancellation, budget, profile or event
routing when using one batch's DispatchOptions across multiple Runs.

## Decision

Wake the matching executor after successful Outbox publication/authorization.
Wake the matching Coordinator after each result is durably published, even
while another Effect in that dispatch batch remains unfinished. Notifications
are coalesced hints: Journal remains authoritative, with claim/lease/fencing
and atomic result consumption unchanged. Streams do not cause Inbox polling.

Use one lazy recovery clock per Engine, shared by its cloned handles. Once per
second, registered readers recheck their own Journal work/results. This is a
low-frequency fallback for missed hints, external writes and expired claims,
not a new global durable scan API or message transport. Recreating an Engine's
execution in another Tokio runtime restarts the recovery clock.

Expose a standard-library cancellation future in Core. Parent cancellation
wakes child waiters; dropping a waiter unregisters it. No runtime or I/O
dependency enters Core. Scheduler and dispatch waits consume this signal.

Register owned RunExecutionContext separately from the executor task. Resolve
by RunId; include owned execution profile/dependencies, cancellation, shared
BudgetHandle, timeout, parallel limit, child execution and event routing.
Merge committed usage monotonically without discarding concurrent reservations.
Reject live duplicate registration; detached generations can drain existing
work without unregistering a replacement generation.

## Consequences

- Idle Journal reads follow the fallback frequency rather than 50Hz per Run.
- Independent publication is recoverable even when its local hint is absent.
- A registry is not durable state: channels, cancellation handles and trusted
  services are restored in-process when a Coordinator resumes.
- This stage retains per-Run workers and live Coordinators. Shared cross-Run
  workers, Run Parking, durable remote context resolution and Sandbox
  reattachment remain future work. Budget sharing here is process-local;
  distributed reservations require an additional atomic protocol.
- Existing persistent cancellation polling and lease renewal remain. Lease
  deadlines and ownership checks are not weakened to pass load tests.

See [implementation and validation](../runtime-wakeup-context.md).

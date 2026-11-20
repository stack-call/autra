# Run Snapshots and Subscriptions Are In-Process

**Status:** accepted

Clients and future UI code had no read model for a live Run: they could collect
`RunEvent`s from the callback or load the journal after completion, but could
not ask for current state and then follow events.

`autra-execution` defines `RunSnapshot` (run id, parent, phase, budget, final
answer, last error, last event sequence, effect count). `autra-runtime` adds
`RunHub`:

- `start(snapshot)` registers a live Run and creates a broadcast channel;
- `publish` forwards each event;
- `update`/`finish` replace the snapshot and close the channel;
- `subscribe(run_id)` returns the snapshot plus a receiver;
- `runs()` lists known snapshots.

`AgentBuilder::run_hub` attaches a hub to the engine. The coordinator publishes
through the hub and updates the snapshot after each committed transition.
`autractl snapshot --journal <path> <run-id>` builds a snapshot from durable
state for cross-process inspection.

Consequences: an in-process client can subscribe before or during a Run, and a
finished Run still returns its final snapshot. `RunHub` is memory-only:
subscriptions do not cross processes or restarts, and the broadcast channel is
best-effort (lagged receivers skip events). JSON snapshot output for the CLI is
a stable read model; a network protocol (`GetRunSnapshot`/`SubscribeRun`) is
future work.

Alternatives considered: polling the journal for live state (delayed and
recovery-oriented), storing the hub in SQLite (not a live stream), and emitting
snapshots as events (duplicates state and events).

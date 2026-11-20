# Sessions Are In-Memory and Single-Writer

**Status:** accepted

Conversations need prior messages, but concurrent turns against one mutable
history would make Run state undefined and hard to test.

We decided a `Session` owns an Agent and the accepted `HistoryItem` list.
`run_turn` requires `&mut self`, so one Session has one writer. Each turn starts
a fresh Run with a fresh budget and Run id, seeded with the Session history.
Completed and Cancelled turns store the outcome history; a failed turn leaves
the previous history unchanged.

Consequences: no concurrent turns in one Session and no persistence across
process exit. Callers who need concurrency create separate Sessions. Durable
sessions belong to the durable host slice.

Alternatives considered: a shared `Arc<Mutex<History>>` (allows concurrent
turns but weakens ordering), a server-side session store (premature), and
rebuilding the Agent per turn (leaks state). Revisit when a durable session
store or a multi-client session protocol is required.

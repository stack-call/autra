# SQLite v0 Also Persists Run Snapshots

**Status:** accepted; extends ADR-0029

Effect history without the Run state cannot explain a completed Run, and the
`runs` table was already part of the target schema.

We added `save_run`/`load_run` to `EffectJournal` with default no-op behavior,
and `SqliteEffectJournal` now upserts the final `AgentState` in a `runs` table.
MemoryJournal stores snapshots in memory. The Agent writes state and effects at
run completion.

Consequences: a Run snapshot and its effects survive reopen. There is still no
event journal, outbox, checkpoint or resume; a crash mid-run loses progress.
Durable recovery can build on the same boundary.

Alternatives considered: waiting for the full Run/event schema (slower), storing
only the state (loses effect detail), and separate journal types (unnecessary
split). Revisit with the run_events and outbox tables.

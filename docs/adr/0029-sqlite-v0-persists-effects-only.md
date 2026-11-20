# SQLite v0 Persists Effects Only

**Status:** accepted

The first durable backend must be small enough to verify before Run recovery,
outbox dispatch and reconciliation exist. Attempting all of them at once would
hide correctness gaps.

We added `autra-durable-sqlite` v0: an `EffectJournal` implementation that
upserts `EffectRecord` JSON by effect id and loads records by run id. The Agent
writes its final effect ledger at run completion via `AgentBuilder::journal`.

Consequences: effect history survives process exit and reopen; Run state, run
events, approvals, the outbox, checkpoints and resume do not. A crash during a
run can still lose progress. The schema and write path are ready for the next
durable slice.

Alternatives considered: full SQLite Run recovery now (too large), a JSONL file
(simpler but not the target), and no persistence (blocks verification). Revisit
when the run/outbox schema lands.

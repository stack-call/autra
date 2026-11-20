# Each Accepted Event Commits a Checkpoint

**Status:** accepted

Persisting only at run completion cannot support recovery; the journal needs a
state and event boundary that matches the coordinator's accepted-event order.

We made `EffectJournal::commit_event` the local durable boundary: it writes the
current `AgentState`, the accepted event and the current EffectRecords in one
SQLite transaction. MemoryJournal keeps the same triple in memory. The Agent
calls it after accepting each transition and before dispatching its effects.

Consequences: the journal has an ordered event log and a checkpoint per accepted
event. It still has no outbox, resume, effect reconciliation or approval
persistence; a crash between effect dispatch and the next event can leave an
ambiguous boundary. Those are the next durable slices.

Alternatives considered: an event-only journal (no fast state load), a
state-only journal (no audit), and a separate checkpoint timer (unnecessary for
local durability). Revisit with the outbox and resume work.

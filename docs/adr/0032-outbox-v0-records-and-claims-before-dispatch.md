# Outbox v0 Records and Claims Before Dispatch

**Status:** accepted

Dispatching an effect directly from the transition commit leaves no durable
record of "intended but not yet executed" work.

We added an `effect_outbox` table and journal APIs: enqueue an Authorized effect
with its intent JSON, claim due entries with a lease, release a claim, and
complete it. MemoryJournal mirrors the protocol. EffectRecords now carry
`intent_json` so a dispatcher can reconstruct the intent.

Consequences: the journal can express the dispatch boundary and duplicate
claims are controlled by a lease. The Agent still dispatches directly and does
not call enqueue/claim yet; the dispatcher loop and at-least-once execution are
the next slice.

Alternatives considered: dispatch directly without an outbox (loses the
boundary), in-memory-only queue (not durable), and exactly-once claims (not
promised). Revisit when the dispatcher is wired.

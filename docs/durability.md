# Durability

Durability is conditional. A guarantee exists only when the chosen execution
host provides it.

## Execution hosts

| Host | Owns | Does not promise | Status |
|---|---|---|---|
| `LocalEphemeral` | In-process state, concurrency, interaction, cancellation | Recovery after process exit | Implemented |
| `NativeDurable` | SQLite journal, transactional outbox, checkpoints, recovery at recorded boundaries | Exactly-once external I/O | Planned |
| `RestateHosted` | Restate journal, workflow/handler, durable waits, service calls | Custom SQLite journal equivalence, automatic workspace persistence | Planned |

The three hosts share Stepper semantics but not recovery, timer or delivery
guarantees. See INV-08.

## What LocalEphemeral guarantees

- One ordered accepted-event stream per Run.
- Tools and model calls are explicit Effects.
- Budgets, cooperative cancellation and optional Effect timeouts.
- A cancelled Run returns a Cancelled outcome; an in-flight Effect may finish.
- No recovery after process exit, and no durable journal.

## NativeDurable plan

The first durable host will use SQLite and an outbox:

1. validate `run.state_version`;
2. write the next state or checkpoint;
3. append ordered durable events;
4. write a unique Effect intent and outbox record;
5. advance sequence/version and commit.

A dispatcher consumes only committed outbox rows and rescans unfinished intents
after restart. External APIs cannot join a local transaction, so the window
between "the external side effect happened" and "its result was committed"
must be explicit.

## Failure windows

| Failure point | Possible fact | Correct response |
|---|---|---|
| Before commit | The intent may not exist | Replay accepted events; no implied duplicate |
| Intent committed, not dispatched | Recorded, dispatch unknown | Use dispatcher claim state |
| Request sent, no result | May have executed | Classify: retry, reconcile, or ask a human |
| Tool succeeded, log not committed | External state changed | OutcomeUnknown; reconcile |
| Result committed, client not notified | Authoritative fact exists | Replay the recorded result |
| Cancel races with completion | Both may be true | Record real order; never fake a rollback |

## Reconciliation

A conditional file write can be checked against the original version and
target hash. A `git push`, email or payment may have no idempotency or query
API; the system must surface an unknown state rather than invent exactly-once.

## Session store vs execution journal

Session persistence and Run execution persistence are different truths. A
Session store needs conversation metadata, turn records, the run association
and a user-visible status summary. A durable execution host needs the committed
Run state, Effect intents, tool results, approvals and the authoritative
journal. They may share a SQLite database, but they must not share one state
machine or one transaction that makes a Session claim a Run exists before the
execution state is committed.

## Current limitation

`autra-durable-sqlite` v0 persists EffectRecords and loads them across reopen;
`AgentBuilder::journal` writes the final ledger at run completion. It does not
persist Run state, events, approvals, an outbox or checkpoints, and it cannot
resume a crashed Run. A timed-out tool is marked `outcome_unknown`, but there is
no reconciliation pass. Do not infer full durable recovery from the journal.

See [`ADR-0003`](adr/0003-default-local-host.md) and
[`ADR-0008`](adr/0008-native-durable-and-restate-independent.md).

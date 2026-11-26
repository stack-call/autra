# Durability

Durability is conditional. A guarantee exists only when the chosen execution
host provides it.

## Execution hosts

| Host | Owns | Does not promise | Status |
|---|---|---|---|
| `LocalEphemeral` | In-process state, concurrency, interaction, cancellation | Recovery after process exit | Implemented |
| `NativeDurable` | SQLite journal, transactional outbox, atomic transition commit, resume at recorded boundaries | Exactly-once external I/O; automatic background recovery | Partial |
| `RestateHosted` | Restate journal, workflow/handler, durable waits, service calls | Custom SQLite journal equivalence, automatic workspace persistence | Planned |

The three hosts share Stepper semantics but not recovery, timer or delivery
guarantees. See INV-08.

## What LocalEphemeral guarantees

- One ordered accepted-event stream per Run.
- Tools and model calls are explicit Effects.
- Budgets, cooperative cancellation and optional Effect timeouts.
- A cancelled Run returns a Cancelled outcome; an in-flight Effect may finish.
- No recovery after process exit, and no durable journal.

## NativeDurable protocol

Each accepted event commits through `DurableTransitionStore::commit_transition`:

1. compare the stored `state_version` with the expected version (CAS);
2. write the next state with `state_version + 1`;
3. append the event with a strict insert, so a duplicate sequence fails;
4. write the new EffectRecords;
5. insert outbox rows for new `Authorized` effects.

`DurableTransitionStore` is a separate contract from `EffectJournal`. A plain
`EffectJournal` has no default `commit_transition`; a host that claims durable
semantics must implement the durable trait explicitly. SQLite performs these
steps in one transaction. A version conflict, duplicate sequence or write
failure rolls the whole transition back: state, event, effects and outbox
cannot disagree about whether the transition happened. `MemoryJournal`
implements the durable trait with in-process version and duplicate checks for
tests; it is not crash-safe.
Effects that need approval are committed as `AwaitingApproval`; their outbox
row is created only after approval, which is a separate commit by design.

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

`autra-durable-sqlite` v0 persists the Run snapshot, EffectRecords and accepted
events; each accepted event commits state, event and effects in one journal
transaction. It also provides an `effect_outbox` with enqueue, claim, release and
complete operations, and the Agent executes through a `Dispatcher` boundary.
`OutboxDispatcher::run_once_for_run` claims due rows for a Run, executes the
intent through the Dispatcher, records the outcome and completes the row. When
a journal is configured, the Agent records and enqueues every Authorized effect
and executes it through that loop; tool batches are claimed together and
dispatched with bounded parallelism. Without a journal the Agent keeps the
`LocalEphemeral` direct path.

`Agent::resume` continues a committed checkpoint. An `AwaitingTool` checkpoint
re-dispatches `Authorized` effects and rejects calls whose approval was never
completed. An in-flight `Dispatched` call is retried only when the tool is
`ReadOnly` or `Idempotent`; any other tool becomes `OutcomeUnknown` instead of
being retried blindly. Reconstructed outcomes are fed back into the stepper. An
`AwaitingModel` checkpoint retires the interrupted model effect and re-issues
the call through a `ResumeRequested` event. Completed and cancelled runs return
their stored outcome. The CLI exposes this as `--journal` and `resume`.

Child Runs can share a durable journal through `ChildExecutionContext`; their
effects and state are recorded under the child Run id with `parent_run_id`.
The parent does not coordinate child recovery, and an interrupted
`spawn_subagent` remains an unknown outcome rather than being retried.

The SQLite adapter opens its databases in WAL mode with a five-second busy
timeout, so the journal and session store can share one file without immediate
`SQLITE_BUSY` failures. `PRAGMA user_version` records the schema version and a
database written by a newer version is refused instead of being misread.

Schema version 3 adds the durable Result Inbox; version 2 is upgraded
transactionally. In pooled execution, result publication retires Outbox in the
same transaction, and acceptance consumes Inbox with the next owned Run
transition. See [runtime refactor](runtime-refactor.md) for lease fencing,
environment binding, migration and compatibility details.

Recovery also checks engine-owned state: a checkpoint whose `engine_id` is not
the active stepper, whose `schema_revision` is newer, or whose payload cannot
be decoded fails the resume. It is never replaced with an empty history.

Failure paths that happen outside a stepper transition (for example a model
transport error) use `commit_terminal` with the expected `state_version` rather
than an unconditional `save_run`. A stale terminal commit is rejected, so a
failed coordinator cannot overwrite a newer state.

Outbox claims carry a 30-second lease. `Agent::drain_outbox` and
`autractl outbox` reclaim due rows. Completing or releasing a row requires the
current claim id, so a worker whose lease expired cannot remove an effect now
owned by another worker. A reclaimed row whose record is already terminal is
only cleaned up, and a reclaimed row whose record is `Dispatched` becomes
`OutcomeUnknown` rather than executing a second time. This closes the crash
window between recording an outcome and deleting its outbox row.

A polling daemon (`autractl daemon --journal <path>`) scans the journal and
resumes runs parked at `AwaitingTool` or `AwaitingModel`, skipping terminal
states. By default it rejects new approvals unless `--yes` is passed. It is an
opt-in process, not part of the library runtime. Approval decisions persist and
`Agent::reconcile` retries safe `OutcomeUnknown` effects. The daemon holds a service lease, so only one daemon
owns a journal, and it skips Runs whose owner lease is still live. Active Runs
renew their Run lease; a crashed owner becomes reclaimable after the lease
expires. Resume also checks the stored Agent profile hash and refuses a
different instructions/tools/policy configuration. See
[`local-runtime-stability.md`](local-runtime-stability.md). A timed-out tool is marked `outcome_unknown`;
safe classes can be reconciled, and unsafe classes stay unresolved for a human. Do not infer
full durable recovery from the journal.

See [`ADR-0003`](adr/0003-default-local-host.md) and
[`ADR-0008`](adr/0008-native-durable-and-restate-independent.md).

The known gaps around leases, daemon ownership, process cancellation, atomic
writes and journal retention are tracked in
[`local-runtime-stability.md`](local-runtime-stability.md).

# A Transition Commits Atomically with Its Effects

**Status:** accepted

The coordinator committed an accepted event and the Run state, then created
the transition's EffectRecords and enqueued them in later calls. A crash
between those calls could leave a committed state whose effects were never
recorded, and the agent had to guess what happened on resume. The SQLite
journal also had no state version check and replaced event rows on conflict,
so two writers or a replay could silently overwrite history.

Every accepted event now commits through
`EffectJournal::commit_transition(expected_version, event, state, new_effects)`.
The SQLite implementation performs one transaction:

1. compare `runs.state_version` with `expected_version` and reject a mismatch;
2. write the next state, whose `state_version` must be `expected_version + 1`;
3. insert the event with a strict `INSERT`, so a duplicate sequence fails;
4. upsert the new EffectRecords;
5. insert outbox rows for new `Authorized` effects.

A failure rolls back all five. `AgentState` carries `state_version`; old rows
deserialize with version 0, and the `runs` table gains the column through a
best-effort migration. The coordinator stages effects before committing and
sets `Authorized` for tools that need no approval; approval-required effects
are staged as `AwaitingApproval` and enter the outbox only after approval.
Cancellation after the commit marks staged effects `CancelledBeforeDispatch`
and removes their outbox rows before the cancel event is processed.

Consequences: state, event, effects and the authorized outbox cannot disagree
about whether a transition happened. Approval remains a separate commit, so a
crash between commit and approval leaves `AwaitingApproval`, which resume
rejects. The default `commit_transition` for in-memory journals checks the
version and duplicates in process, but only the SQLite host provides a real
transaction. Event history is now append-only at the SQLite layer.

Alternatives considered: keeping the multi-call sequence and relying on resume
heuristics (the gap the review identified), and making the whole agent loop one
giant transaction (it would hold a database lock across model and tool calls).

# Terminal States Use Versioned Commits

**Status:** accepted.

## Context

Normal Run progress commits through `commit_transition`, which checks
`state_version`. Runtime failures that happen outside a stepper transition —
for example a model transport error after the `AwaitingModel` transition was
already committed — used an unconditional `save_run`. That could overwrite a
newer state, and it made failure persistence weaker than success persistence.

## Decision

`DurableTransitionStore` gains `commit_terminal(run_id, expected_version, state)`:

- the next state must have `state_version == expected_version + 1`;
- the host must reject a stale `expected_version`;
- SQLite performs the check and update in one transaction;
- `MemoryJournal` performs the same version check in memory.

The engine uses it for:

- stepper transition errors;
- rejected transition proposals;
- model execution errors.

Successful, cancelled and `Disposition::Failed` paths already commit through
`commit_transition`.

## Consequences

- Failure persistence has the same CAS protection as normal transitions.
- A stale coordinator cannot overwrite a newer terminal state.
- A failure before the first commit may insert version 1 if the store allows
  `expected_version == 0`; otherwise it is rejected as a conflict.
- Custom journals get a default implementation that delegates to `save_run`, but
  durable hosts that claim optimistic concurrency should override it.

See ADR-0040 (atomic transition commit) and ADR-0056 (transition validation).

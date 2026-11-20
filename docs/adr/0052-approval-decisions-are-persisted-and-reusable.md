# Approval Decisions Are Persisted and Reusable

**Status:** accepted

The runtime asked the approver on every gated tool call. A journaled Run that
crashed while `AwaitingApproval` was rejected on resume, even if a human had
already approved the same call; headless runs had no way to pre-approve an
effect. Approval was the remaining gap in the durable loop.

`DurableTransitionStore` now exposes an `ApprovalStore`:

- `ApprovalRecord` binds effect id, Run id, call id, tool name, argument
  digest, policy revision, actor, decision, decision time and expiry;
- a record is reusable only when the effect, digest and policy revision match
  and it has not expired;
- `MemoryJournal` stores records in process; `SqliteEffectJournal` adds an
  `approvals` table, kept separate from the execution facts.

The engine first loads a usable record; only if none exists does it call the
approver, and it saves every new decision. On resume, `AwaitingApproval` and
`Recorded` pending calls are resolved from a stored decision or re-asked with
the current approver, then dispatched or rejected. `autractl approve` and
`autractl reject` write a decision for a specific effect, using the same policy
revision and actor fields.

Consequences: a crash after approval can continue without asking again; an
expired record, changed arguments or changed policy revision forces a new
decision. Actor identity is still claimed, not verified, and approvals are
scoped to one effect. `spawn_subagent` is still non-idempotent, so an
interrupted spawn remains unknown even if its approval is stored.

Alternatives considered: storing approvals as execution effects (mixes two
truths and complicates recovery), never reusing decisions (safe but poor UX),
and reusing any approved record for the tool (ignores arguments and policy
changes).

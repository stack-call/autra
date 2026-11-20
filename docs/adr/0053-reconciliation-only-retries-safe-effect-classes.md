# Reconciliation Only Retries Safe Effect Classes

**Status:** accepted

Effects that timed out or were interrupted were marked `OutcomeUnknown` and
stayed there. Resuming did not retry them, and there was no operator path to
inspect or clear them.

`LocalEngine::reconcile` (and `Agent::reconcile`) now inspects a Run's
`OutcomeUnknown` effects and retries only when the class makes it safe:

| Effect | Decision |
|---|---|
| Model call | re-issued when model retry is enabled |
| `ReadOnly` or `Idempotent` tool | retried |
| `Idempotent` tool that requires approval | retried only with a valid persisted approval |
| `Reconcileable` tool | unresolved; a tool-specific check is required |
| `NonIdempotent` or `Unknown` tool | unresolved; a human decides |
| Tool no longer registered | unresolved |
| Any tool denied by the current policy | unresolved |

Each retried effect gets a reconciliation note, is set back to `Authorized`,
recorded and enqueued through the outbox. The report lists retried ids and
unresolved ids with reasons. `retry_models` can be disabled for tool-only
reconciliation. Reconciliation updates the effect ledger only; it does not
rewrite Run history.

CLI: `autractl effects --unknown` lists pending unknowns and
`autractl reconcile` runs the pass (model retries only with `--model`).

Consequences: unknown outcomes have an explicit, conservative path. The
decision is per effect class, not a tool-specific query API; `Reconcileable`
tools still need a future `Tool::reconcile` capability. Retrying an effect does
not roll back an earlier unknown side effect, so the operator is still
responsible for outcomes that cannot be verified.

Alternatives considered: retrying every unknown effect (duplicates
non-idempotent side effects), leaving unknown effects forever (current gap),
and adding tool-specific reconciliation callbacks immediately (premature before
a policy registry for reconciliation exists).

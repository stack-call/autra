# Restate Integration

**Status:** not implemented. This page records the intended boundary.

Restate is a **durable execution host**, not a storage backend. It owns the
authoritative journal of a workflow invocation. Autra must not keep a second
SQLite journal that independently decides execution order.

## Intended mapping

```text
Restate workflow / invocation (authoritative journal)
        ↓
Autra Stepper + harness
        ↓
Restate-journaled model/tool activities
        ↓
Trusted tool host / sandbox / external APIs
```

The adapter must define:

- `run_id ↔ workflow_id`;
- approval and input signals;
- version pinning;
- the non-deterministic boundary of context construction;
- Tool Effect identity;
- workspace references.

## Guarantees and gaps

Restate provides durable steps, reliable waits and invocation recovery. It does
not make an external email, payment or `git push` exactly-once. A `ctx.run`
records a result; the window between "the side effect happened" and "the result
was saved" still needs idempotency or reconciliation.

Restate also does not persist the Workspace. A recovered invocation with a lost
workspace is a different environment.

## Development stance

Native local/durable semantics come first. A Restate PoC follows only after the
same scenarios are clear locally, and it must publish a capability comparison
rather than claiming lossless host switching. See
[`ADR-0008`](../adr/0008-native-durable-and-restate-independent.md).

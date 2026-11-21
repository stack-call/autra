# Runtime Evolution: Coordination, Lifecycle and Execution Middleware

**Status: design proposal. The target structures and APIs below are not
implemented.** This page records a design review of the current Runtime and the
intended direction. It does not change the execution semantics described in
[`execution-model.md`](execution-model.md) or [`durability.md`](durability.md).

## Decision summary

Keep three extension powers separate:

| Power | Question | Timing | May change execution? |
|---|---|---|---|
| **Transform** | What request should be proposed? | before commit | yes, restricted draft |
| **Lifecycle** | What happened? | after the fact | no |
| **Middleware (Wrap)** | How is an operation executed? | around dispatch | yes, inside the host boundary |

Trusted authorization (policy and approval) is not ordinary middleware. It is
the gate between a proposal and a committed Effect. A single component owns
`AgentState`; helpers may propose or persist, but they must not each mutate run
state independently.

---

## 1. Current snapshot

The pieces already exist, but their boundaries are implicit:

| Concern | Today | Location |
|---|---|---|
| Run loop, transitions, commit, dispatch, streaming, cancellation | one `LocalEngine::drive` | `crates/autra-runtime/src/engine/mod.rs` |
| Request transformation | `ModelRequestTransform` over `ModelRequestDraft` | `crates/autra-harness/src/middleware.rs` |
| Lifecycle observation | `ObserverHook` (read-only, four stages) | `crates/autra-execution/src/hooks.rs` |
| Execution boundary | `Dispatcher` / `OutboxDispatcher` | `crates/autra-runtime/src/dispatcher.rs` |
| Authorization | `PolicyChain` + `Approver` | `crates/autra-execution/src/policy.rs`, runtime approval module |

Known timing gap: `ObserverHook::on_effect_accepted` is emitted **before**
`commit_transition` (`engine/mod.rs` emits it while building EffectRecords, and
the journal commit happens later). It is therefore closer to
`on_effect_proposed` than to a committed fact. The target model below splits
these events and moves the committed notification after a successful commit.

---

## 2. `drive()` is doing too much

`drive()` currently owns:

```text
Event loop
Transition
Effect recording
Journal commit
Tool batches
Model dispatch
Streaming
Cancellation
Run completion
```

This is not automatically wrong: one Run needs one clear control center. The
problem is that every new feature edits the same function, and the ordering
rules between commit, authorization and dispatch become implicit.

### Proposed internal decomposition

```text
RunCoordinator
    ├── TransitionCommitter
    ├── EffectScheduler
    ├── AuthorizationGate
    └── ResultCollector
```

| Collaborator | Responsibility | Must not |
|---|---|---|
| `RunCoordinator` | event loop, cancellation, completion, ordering | mutate state owned by other collaborators |
| `TransitionCommitter` | validate proposal, CAS, atomic commit | dispatch Effects |
| `EffectScheduler` | build records, enqueue outbox, group batches | approve Effects |
| `AuthorizationGate` | policy + approval, bind digest/actor/expiry | execute Effects |
| `ResultCollector` | collect dispatch results, stream deltas, enqueue accepted events | commit transitions |

**Invariant:** only the coordinator writes the authoritative `AgentState`.
Collaborators receive narrow inputs and return proposals/results. This keeps
one state owner while allowing mechanisms to be tested and replaced
individually.

The decomposition is a long-term direction, not a required first step.

---

## 3. Execution middleware belongs at the dispatch boundary

Target pipeline:

```text
Runtime::drive()
       │
       ├── Lifecycle: RunStarted
       │
       ▼
Stepper.transition()
       │
       ▼
TransitionProposal
       │
       ▼
RequestTransform            (before commit)
       │
       ▼
Policy / Approval           (trusted authorization)
       │
       ▼
Commit state + Effect
       │
       ├── Lifecycle: EffectCommitted
       │
       ▼
Execution Middleware        (wrap around dispatch)
       │
       ├── Middleware A.before
       │     └── Middleware B.before
       │           └── Dispatcher
       │                 └── Model / Tool
       │           └── Middleware B.after
       └── Middleware A.after
             │
             ▼
Record Effect result
             │
             ├── Lifecycle: EffectResolved
             │
             ▼
AcceptedEvent → drive()
```

Ordering rules:

- Registration order for `before`, reverse order for `after`.
- `next` is callable **once**; a chain cannot repeat an operation implicitly.
- Middleware may transform inputs/outputs only inside the execution boundary; it
  must not replace the approved Effect arguments or its digest.
- Retrying a non-idempotent tool is not a middleware decision. Retry and
  reconciliation belong to the runtime/host policy and the Effect class rules.
- A middleware error becomes the Effect outcome; it cannot leave the journal
  unaware of an external call that already happened.
- Streaming deltas pass through the chain, but must not be duplicated or
  replayed after a partial stream.
- Cancellation is observed at the same boundary; a middleware must not swallow
  it.

Transforms are different: they run **before** the Effect is committed and only
see `ModelRequestDraft` (system, messages, tools). They cannot touch
`AgentState`, and they cannot authorize anything.

Policy and approval are also different: they are the trusted gate, not a
replaceable wrap. Making them middleware would let an ordinary extension widen
its own authority.

---

## 4. Lifecycle and Middleware are separate

They are not separated merely because their timing differs; they own different
**control**.

| Dimension | Lifecycle | Middleware (Wrap) |
|---|---|---|
| Purpose | observe execution | extend execution |
| Changes the flow | no | yes, within authority |
| Changes input/output | no | yes, inside the boundary |
| Calls `next()` | no | yes |
| Can block execution | no | yes, with explicit semantics |
| Failure handling | usually does not affect the Run | may affect the current operation |
| Typical use | logs, metrics, notifications | caching, tracing, result mapping, rate limiting |

Why not implement lifecycle as middleware, even though it is technically
possible:

1. **Observation must not gain a veto.** If an observer holds `next()`, it can
   silently prevent an operation.
2. **Notifications need fixed semantic boundaries.** `EffectCommitted` must mean
   the commit succeeded. A middleware `before` hook has no such meaning and
   changes meaning with its position in the chain.
3. **Lifecycle spans more than one operation.** A Run may start, call a model,
   request approval, exit, resume in another process, call a tool and finish. A
   middleware wrapping one dispatch cannot express that.

Lifecycle is therefore published by the runtime at defined state changes;
middleware wraps dispatch.

---

## 5. Timing and delivery guarantees

Lifecycle events fire **after the corresponding fact holds**:

```text
Stepper produces EffectIntent
          │
          ▼
Runtime.commit_transition()
          │
     commit succeeds
          │
          ▼
on_effect_committed()
```

Never announce `EffectCommitted` before the journal commit. If the commit fails,
observers must not have received a fact that did not happen.

Proposed event set:

```rust
pub trait LifecycleObserver: Send + Sync {
    fn on_run_started(&self, event: &RunStarted) {}
    fn on_effect_proposed(&self, event: &EffectProposed) {}
    fn on_effect_committed(&self, event: &EffectCommitted) {}
    fn on_effect_resolved(&self, event: &EffectResolved) {}
    fn on_run_finished(&self, event: &RunFinished) {}
}
```

Semantics:

| Event | Fires when | Notes |
|---|---|---|
| `on_run_started` | run state is accepted | before the first model call |
| `on_effect_proposed` | stepper proposed an EffectIntent | pure intent, not yet authorized or committed |
| `on_effect_committed` | the transition committed | the durable fact exists |
| `on_effect_resolved` | the Effect result is recorded | terminal or `OutcomeUnknown` |
| `on_run_finished` | terminal state committed | completed, failed or cancelled |

In-memory callbacks are **best effort**. If a notification must be reliable, it
belongs in the Journal/Outbox, not in a lifecycle callback. Observers cannot
mutate accepted execution.

---

## 6. Target layout

Suggested file layout; names are illustrative, not current:

```text
autra-harness/
└── middleware/
    └── model_transform.rs

autra-execution/
├── lifecycle.rs
│   └── LifecycleObserver
└── middleware.rs
    ├── ExecutionMiddleware
    └── Next

autra-runtime/
├── lifecycle.rs
│   └── LifecycleEmitter
└── middleware/
    ├── chain.rs
    └── next.rs
```

Sketch of the wrap trait:

```rust
#[async_trait]
pub trait ExecutionMiddleware: Send + Sync {
    async fn handle(
        &self,
        request: ExecutionRequest,
        next: Next<'_>,
    ) -> ExecutionResult;
}
```

`Next` should enforce:

- callable once;
- carries the already-authorized Effect identity and digest;
- cannot be replaced by a fresh request with different arguments;
- surfaces cancellation and timeout rather than hiding them.

---

## 7. Migration plan

### Phase 1 — Lifecycle correctness

1. Split `on_effect_accepted` into `on_effect_proposed` and
   `on_effect_committed`.
2. Emit `on_effect_committed` only after `commit_transition` succeeds.
3. Emit `on_effect_resolved` after the result commit.
4. Keep `ObserverHook` as a compatibility shim during the transition.
5. Add tests proving no committed notification on commit failure.

### Phase 2 — Execution middleware chain

1. Introduce a middleware chain at the `Dispatcher` boundary with an identity
   default.
2. Add tracing/metrics middleware as the first consumer.
3. Test ordering, once-only `next`, error mapping, digest immutability and
   cancellation.
4. Keep model-request transforms in the pre-commit phase.

### Phase 3 — Coordinator decomposition

1. Extract `TransitionCommitter`, `AuthorizationGate`, `EffectScheduler` and
   `ResultCollector` behind private interfaces.
2. Keep the public `Agent`/`RunOptions` surface unchanged.
3. Prove behavior with the existing runtime and durability suites.
4. Only after that consider exposing the collaborators as extension points.

---

## 8. Non-goals

- No plugin manager, dynamic loading or marketplace in this design.
- No unified `Extension` trait that folds Observe, Intercept and Transform
  together.
- No lifecycle observer with the ability to block or mutate.
- No middleware that bypasses policy, approval or the journal.
- No automatic retry of non-idempotent tools.
- No second owner of `AgentState`.

---

## 9. Open questions

- Where do provider-level retries end and execution middleware begin?
- Can streaming deltas be transformed (for example redaction) without breaking
  the recorded result?
- How do child-run lifecycle events relate to the parent's stream?
- What is the ordering between approval resolution, resume and middleware
  `before` hooks?
- Should middleware be allowed to short-circuit with a synthetic result, and how
  is that recorded?
- Which tests need an error-injection middleware to prove the failure windows?

---

## References

- [`execution-model.md`](execution-model.md) — Run, Effect, commit and recovery
  semantics.
- [`durability.md`](durability.md) — journal, outbox and failure windows.
- [`extensions.md`](extensions.md) — current extension points.
- [`extension-architecture-v1.md`](extension-architecture-v1.md) — broader
  extension-system proposal.
- ADR-0002 (small sans-I/O core), ADR-0007 (tool/effect/authority separation),
  ADR-0028 (transition is a proposal), ADR-0040 (atomic transition commit),
  ADR-0046 (typed model-request middleware).

# Runtime Evolution: Responsibility, Lifecycle and Execution Middleware

**Status: design proposal. The target structures and APIs below are not
implemented.** This page records a design review of extension placement and
Runtime structure. It does not change the execution semantics in
[`execution-model.md`](execution-model.md) or [`durability.md`](durability.md).

**Implementation update:** the lifecycle split and its commit-time ordering, the
prepare/commit/apply boundary, and the `EffectExecutor` extraction are now
implemented. The execution middleware chain and the remaining coordinator
decomposition are still proposals. See
[`extension-system-review.md`](extension-system-review.md) for the current
security and consistency review.

## Decision summary

Two dimensions are often conflated:

- **Mechanism**: Transform, Lifecycle, Wrap Middleware.
- **Layer**: Harness, Runtime, Execution contracts.

They are not one-to-one. The placement of an extension is decided by two
questions:

1. **What does it change?** Prompt, context or visible tools belong to the
   Harness. External execution, authorization and dispatch belong to the
   Runtime. Permission decisions belong to trusted Runtime policy, not ordinary
   middleware.
2. **What fact does it observe?** Agent decisions can be observed at the
   Harness behaviour boundary. Effect commit, authorization, dispatch, result
   and Run completion can only be published by the Runtime.

The resulting split is:

| | Harness | Runtime |
|---|---|---|
| **Transform** | `ModelRequestTransform` over an uncommitted draft | must not rewrite committed Effects |
| **Lifecycle** | optional behaviour-phase observation; must not claim execution facts | authoritative Run and Effect lifecycle |
| **Wrap** | possible around the stepper's decision algorithm; not needed yet | around `Dispatcher` at the real execution boundary |

Do not implement two identical Lifecycle + Middleware frameworks in both
layers. Keep the Harness lightweight; let the Runtime own authoritative
lifecycle and the execution wrap.

The non-negotiable boundary is not directory placement. It is this:

> **The ability to change an agent's decision and the ability to change an
> already-authorized execution must not cross.**

---

## 1. Current snapshot

| Concern | Today | Location |
|---|---|---|
| Run loop, transition, commit, dispatch, streaming, cancellation | one `LocalEngine::drive` | `crates/autra-runtime/src/engine/mod.rs` |
| Model request transformation | `ModelRequestTransform` over `ModelRequestDraft` | `crates/autra-harness/src/middleware.rs` |
| Lifecycle observation | `ObserverHook` (read-only, four stages) | `crates/autra-execution/src/hooks.rs` |
| Execution boundary | `Dispatcher` / `OutboxDispatcher` | `crates/autra-runtime/src/dispatcher.rs` |
| Authorization | `PolicyChain` + `Approver` | `crates/autra-execution/src/policy.rs`, runtime approval module |

`MiddlewareStepper::transition` applies `ModelRequestTransform` to
`EffectIntent::CallModel` **inside the stepper**, before the transition is
committed. It is a pre-commit transform, not an execution wrap.

Known timing gap: `ObserverHook::on_effect_accepted` is emitted **before**
`commit_transition` (`engine/mod.rs` emits it while building EffectRecords; the
commit happens later). It is closer to `on_effect_proposed` than to a committed
fact. The target model below splits these events.

---

## 2. Two dimensions, three mechanism families

### 2.1 Mechanism definitions

| Mechanism | Question | Timing | May change execution? |
|---|---|---|---|
| **Transform** | What should be proposed? | before commit | yes, on a restricted draft |
| **Lifecycle** | What happened? | after the fact | no |
| **Wrap Middleware** | How is this operation executed? | around dispatch | yes, inside the host boundary |

Trusted authorization (policy and approval) is a fourth thing, and not a
replaceable middleware: it is the gate between a proposal and an authorized
Effect.

### 2.2 Placement matrix

| Mechanism | Harness | Runtime |
|---|---|---|
| Transform | **yes** — prompt, context, visible tools, model request draft | no rewriting of committed Effect intents |
| Lifecycle | optional **behaviour-phase** observation (`before_step`, `after_step`), if ever needed | **authoritative** Run/Effect/Attempt lifecycle |
| Wrap | possible around the stepper's decision algorithm; no current use case | **yes** — around `Dispatcher` execution |

The Execution crate owns shared contracts (`LifecycleObserver`,
`ExecutionMiddleware`); it does not own the runtime's events or the harness's
call sites.

---

## 3. Why the Harness needs extension

The Harness manages agent behaviour:

```text
user input
   ↓
build model context
   ↓
decide CallModel
   ↓
receive ModelResult
   ↓
decide CallTool
```

Typical needs:

- add a system-prompt fragment;
- filter the tools visible to the model;
- adjust context selection for the current task;
- inspect a model decision before it becomes an Effect.

These are **behaviour extensions**. They belong at the Harness decision
boundary, not in the Runtime dispatch path.

Current mechanism:

```rust
fn transform(
    &self,
    ctx: &ModelBuildContext,
    draft: ModelRequestDraft,
) -> Result<ModelRequestDraft, ExtensionError>;
```

`MiddlewareStepper` applies transforms after the inner stepper produces a
`CallModel` intent and before the Runtime commits the Effect. The transform
only sees `ModelRequestDraft`; it never receives `&mut AgentState`.

The Harness could later gain `before_step` / `after_step` observation around
decisions. That would be a **Harness behaviour lifecycle**, not the Runtime's
`EffectCommitted` / `RunCompleted` lifecycle.

---

## 4. Why the Runtime needs extension

The Runtime manages execution facts:

```text
TransitionProposal
   ↓
Effect commit
   ↓
Authorization
   ↓
Dispatcher
   ↓
Model / Tool
   ↓
Effect result
```

Typical needs:

- measure real model/tool latency;
- trace tool execution;
- audit after a Run finishes;
- count Effect attempts;
- publish run status.

The Harness cannot know whether an Effect was committed, approved, dispatched,
recovered or resolved. Therefore:

```text
Harness
    └── EffectProposed
              │
              ▼
Runtime
    ├── EffectCommitted
    ├── ApprovalRequested
    ├── EffectAuthorized
    ├── EffectRejected
    ├── EffectDispatched
    ├── EffectResolved
    └── RunCompleted
```

If a lifecycle event is about authoritative execution, the publisher must be
the Runtime.

---

## 5. Two questions decide placement

**First: what does it change?**

| Change | Owner |
|---|---|
| Prompt, context, model-visible tools | Harness transform |
| How an external call is scheduled or executed | Runtime wrap |
| Whether an Effect is permitted | trusted Runtime policy/approval |

**Second: what fact does it observe?**

| Fact | Publisher |
|---|---|
| What decision the agent produced | Harness behaviour boundary |
| Whether an Effect committed, was authorized, dispatched or resolved | Runtime |
| Whether a Run completed, failed, cancelled or resumed | Runtime |

This is stricter than “Lifecycle goes in the Runtime, Middleware goes in the
Harness”. Both layers may have more than one mechanism; they must not have the
same authority.

---

## 6. Authoritative lifecycle and event timing

### 6.1 Effect lifecycle

An Effect may be committed before it is authorized. Approval-required Effects
are recorded as `AwaitingApproval` first, then authorized or rejected.

Proposed order:

```text
Stepper.produces EffectIntent
          │
          ▼
Runtime builds EffectRecords
          │
          ▼
commit_transition()                 ← CAS + atomic commit
          │
          ├── Lifecycle: EffectCommitted
          │
          ▼
Policy / Approval
          │
          ├── Lifecycle: EffectAuthorized / EffectRejected
          │
          ▼
Execution Middleware chain
          │
          ▼
Dispatcher → Model / Tool
          │
          ▼
Record Effect result
          │
          ├── Lifecycle: EffectResolved
          │
          ▼
AcceptedEvent → drive()
```

`EffectCommitted` means the Effect record is part of the authoritative run
state. With a journal it means the transaction succeeded; without a journal it
means local acceptance. It does **not** mean the external call happened.

Never announce a fact before it holds:

```text
commit_transition succeeds
        ↓
on_effect_committed()
```

If the commit fails, observers must not have received a fact that did not
happen. In-memory callbacks are best effort; anything that must be delivered
reliably belongs in the Journal/Outbox, not in a lifecycle callback.

### 6.2 Proposed observer interface

```rust
pub trait LifecycleObserver: Send + Sync {
    fn on_run_started(&self, event: &RunStarted) {}
    fn on_effect_proposed(&self, event: &EffectProposed) {}
    fn on_effect_committed(&self, event: &EffectCommitted) {}
    fn on_effect_authorized(&self, event: &EffectAuthorized) {}
    fn on_effect_rejected(&self, event: &EffectRejected) {}
    fn on_effect_resolved(&self, event: &EffectResolved) {}
    fn on_run_finished(&self, event: &RunFinished) {}
}
```

Observers cannot mutate accepted execution and cannot block an operation.

### 6.3 Current gap

`ObserverHook` exposes `on_effect_accepted`, `on_model_request`,
`on_run_started` and `on_run_finished`. `on_effect_accepted` currently fires
before the commit. The migration must split it and fix the timing; the old
method can remain as a compatibility shim during the transition.

---

## 7. Execution middleware belongs at the dispatcher

The Harness stepper is synchronous:

```rust
fn transition(&self, state: &AgentState, event: &AcceptedEvent)
    -> Result<TransitionProposal, StepError>;
```

It computes a decision; it does not await a model or tool. Wrapping it wraps
**decision computation**:

```text
before transition
    Stepper.transition()
after transition
```

The Runtime dispatch path is asynchronous and wraps **real execution**:

```text
before execution
    Dispatcher.call_tool() / call_model()
after execution
```

There is no current need for a generic stepper-wrap system. The natural place
for a real `next()` chain is the `Dispatcher` boundary.

Sketch:

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

Rules for the chain:

- `before` runs in registration order; `after` runs in reverse order.
- `next` is callable **once**.
- Middleware cannot replace the authorized Effect identity or argument digest.
- Middleware must not retry non-idempotent tools. Retry and reconciliation are
  Runtime/host policy decisions based on `EffectClass`.
- Middleware errors become the Effect outcome; they cannot leave the journal
  unaware of an external call that already happened.
- Streaming deltas may pass through the chain, but must not be duplicated or
  replayed after a partial stream.
- Cancellation and timeout must surface, not be swallowed.
- The chain must not bypass policy or approval; those happen before it.

---

## 8. Target architecture

```text
                    Agent::run()
                         │
                         ▼
                  Runtime.drive()
                         │
                         ├── Lifecycle: RunStarted
                         │
                         ▼
                Harness.ReActStepper
                         │
                         ▼
                 ContextBuilder
                         │
                         ▼
              ModelRequestTransform
                         │
                         ▼
                TransitionProposal
                         │
                         ▼
                Runtime Coordinator
                         │
                 Prepare / Commit
                         │
                         ├── Lifecycle: EffectCommitted
                         │
                         ▼
                  Policy / Approval
                         │
                         ├── Lifecycle: EffectAuthorized / Rejected
                         │
                         ▼
               Execution Middleware
                         │
                    Wrap / Next
                         │
                         ▼
                    Dispatcher
                         │
                    Model / Tool
                         │
                         ▼
                    EffectResult
                         │
                   Record Result
                         │
                         ├── Lifecycle: EffectResolved
                         │
                         ▼
                    AcceptedEvent
                         │
                         └────→ drive()
```

Authorization persistence and outbox claim mechanics are omitted to keep the
responsibility boundaries visible.

Suggested code layout; illustrative, not current:

```text
autra-harness/
├── react.rs
├── context.rs
└── transform/
    └── model_request.rs

autra-execution/
├── lifecycle.rs
│   └── LifecycleObserver trait
└── middleware.rs
    └── ExecutionMiddleware trait

autra-runtime/
├── engine/
│   ├── mod.rs
│   └── lifecycle.rs
└── dispatcher/
    ├── mod.rs
    └── middleware.rs
```

`autra-execution` owns shared interfaces. Harness and Runtime provide their own
call sites and implementations.

---

## 9. Coordinator decomposition

`drive()` currently owns the event loop, transitions, effect recording, journal
commit, tool batches, model dispatch, streaming, cancellation and completion.

One control center per Run is desirable; the problem is that every new feature
edits the same function and ordering rules become implicit.

Long-term internal split:

```text
RunCoordinator
    ├── TransitionCommitter
    ├── EffectScheduler
    ├── AuthorizationGate
    └── ResultCollector
```

| Collaborator | Responsibility | Must not |
|---|---|---|
| `RunCoordinator` | event loop, cancellation, completion, ordering | mutate state owned by another collaborator |
| `TransitionCommitter` | validate proposal, CAS, atomic commit | dispatch Effects |
| `EffectScheduler` | build records, enqueue outbox, group batches | approve Effects |
| `AuthorizationGate` | policy + approval, bind digest/actor/expiry | execute Effects |
| `ResultCollector` | collect results, stream deltas, enqueue accepted events | commit transitions |

**Invariant:** only the coordinator writes authoritative `AgentState`.
Collaborators receive narrow inputs and return proposals/results. The public
`Agent`/`RunOptions` surface stays unchanged.

This decomposition is not a prerequisite for the lifecycle or middleware work.

---

## 10. Migration plan

### Phase 1 — Lifecycle correctness

1. Split `on_effect_accepted` into `on_effect_proposed` and
   `on_effect_committed`.
2. Emit `on_effect_committed` only after `commit_transition` succeeds.
3. Add `on_effect_authorized` / `on_effect_rejected` at the authorization gate.
4. Emit `on_effect_resolved` after the result commit.
5. Keep `ObserverHook` as a compatibility shim during the transition.
6. Test that a failed commit emits no committed event.

### Phase 2 — Execution middleware chain

1. Add a `next()` chain at the `Dispatcher` boundary with an identity default.
2. Add tracing/metrics middleware as the first consumer.
3. Test ordering, once-only `next`, error mapping, digest immutability,
   cancellation and non-idempotent retry refusal.
4. Keep model-request transforms in the pre-commit Harness phase.

### Phase 3 — Coordinator decomposition

1. Extract the four collaborators behind private interfaces.
2. Keep the public API stable.
3. Prove behavior with the existing runtime and durability suites.

### Phase 4 — Harness behaviour observation, only if needed

If a real use case appears for `before_step` / `after_step`, add it inside the
Harness. Do not fold it into the Runtime lifecycle and do not call execution
facts from it.

---

## 11. Non-goals and anti-patterns

- No plugin manager, dynamic loading or marketplace in this design.
- No single `Extension` trait that folds Observe, Intercept and Transform
  together.
- No duplicate Lifecycle + Middleware framework in Harness and Runtime.
- No lifecycle observer that can block or mutate.
- No Harness event claiming `EffectCommitted`, `EffectDispatched` or
  `RunCompleted`.
- No Runtime transform that silently rewrites a committed Effect.
- No middleware bypassing policy, approval or the journal.
- No automatic retry of non-idempotent tools in middleware.
- No second owner of `AgentState`.
- No announcing a fact before the corresponding commit succeeds.

---

## 12. Open questions

- Where do provider-level retries end and execution middleware begin?
- Can streaming deltas be transformed (for example redaction) without breaking
  the recorded result?
- How do child-run lifecycle events relate to the parent's stream?
- What is the ordering between approval resolution, resume and middleware
  `before` hooks?
- May middleware short-circuit with a synthetic result, and how is that
  recorded?
- Which tests need an error-injection middleware to prove failure windows?
- Does a journal-less `LocalEphemeral` run treat local acceptance as
  `EffectCommitted`, and should the event name say so?

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

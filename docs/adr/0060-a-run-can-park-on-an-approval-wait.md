# A Run Can Park on an Approval Wait

**Status:** proposed

## Context

An approval wait is currently a suspended future inside the coordinator. When an
Effect needs approval, `resolve_approval` calls `Approver::approve` and awaits
it. The Run keeps its coroutine, its Run lease and its task alive for as long as
the human takes to answer.

Three consequences follow:

1. **A Run cannot outlive its process.** If the process exits while waiting, the
   Run is only recoverable through `resume`, which re-asks the approver. There is
   no way to say "this Run is waiting on a decision" as a durable state.
2. **The daemon cannot wait.** `RecoverySupervisor` scans `AwaitingTool` Runs and
   the CLI daemon passes a `RejectApprovals` approver, so a background Run either
   gets rejected or blocks the scan.
3. **Callers cannot tell "waiting" from "running".** `RunStatus` has only
   `Completed` and `Cancelled`, and `RunOutcome::status` panics for any other
   phase, so a parked Run has no representation at the API boundary.

`EffectStatus::AwaitingApproval` already exists and is persisted. The missing
piece is not the fact — it is the control flow and the return path.

## Decision

A Run parks when an approval is deferred. Parking means: the coordinator stops,
the Run lease is released, and the Run is resumed later by an approval decision.

### 1. Approval has three outcomes

```rust
pub enum ApprovalOutcome {
    Approved,
    Rejected,
    /// No decision yet. The Run should park instead of waiting inline.
    Deferred,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalOutcome;
}
```

`ApprovalDecision` (two-state) stays as the persisted record type. Only decided
approvals are stored, so `ApprovalRecord.decision` never holds `Deferred`.

An approver that wants the old blocking behavior simply awaits internally and
returns `Approved` or `Rejected`; `Deferred` is opt-in.

### 2. Parking is expressed without a new Core phase

`AgentPhase` keeps `AwaitingTool`. A parked Run is:

```text
phase == AwaitingTool
and at least one Effect is AwaitingApproval
```

This avoids changing the legal phase graph, `validator`, or any Stepper.

### 3. `RunStatus` gains `AwaitingApproval`

```rust
pub enum RunStatus {
    Completed,
    Cancelled,
    AwaitingApproval,
}
```

`RunOutcome` is relaxed from "only Completed or Cancelled" to include a parked
Run. `RunOutcome::status` maps a parked state to `AwaitingApproval` and keeps
panicking for `Failed`, `Ready`, `AwaitingModel`, which never produce an outcome.

A parked outcome carries the Run id so the caller can resume it:

```text
RunOutcome::status()   == AwaitingApproval
RunOutcome::state()    -> phase AwaitingTool, Effects AwaitingApproval
```

### 4. Resume is the wakeup

No new event type is required for the first version:

```text
parked Run
  -> decision written to the approval store (CLI approve/reject, TUI, API)
  -> resume (explicit or via RecoverySupervisor)
  -> resolve_pending_approvals finds a usable decision
  -> Effect becomes Authorized
  -> Run continues
```

`resolve_pending_approvals` already reuses a persisted decision when digest,
policy revision and expiry match, and re-asks otherwise. Parking reuses that
path instead of adding a second one.

### 5. Recovery must not busy-loop on parked Runs

`recover_once` scans `AwaitingTool`, which includes parked Runs. Before resuming,
it checks whether the Run has at least one usable approval decision or an
approver that can decide now. A parked Run with no decision and no approver is
counted as `skipped`, not `resumed`, so the daemon does not re-park it every
pass.

### 6. Parking releases the Run lease

The coordinator returns, so `RunLeaseGuard` drops and the lease is released.
The Run is claimable again by `resume`. Two supervisors cannot both resume it
because the lease is exclusive.

## Consequences

- An approval wait no longer holds a coroutine, a task or a lease.
- `RunStatus` gains a variant; callers matching exhaustively must handle it.
- `Approver::approve` changes its return type; existing approvers need a
  one-line change (`ApprovalDecision::Approved` becomes
  `ApprovalOutcome::Approved`).
- `RunOutcome` can now describe a non-terminal Run. Code that assumed "outcome
  means finished" must check `status()`.
- The TUI's approver keeps blocking and returning `Approved`/`Rejected`, so
  interactive behavior is unchanged.
- `autractl approve|reject` already writes decisions; it becomes the documented
  way to unblock a parked Run.
- A parked Run still holds its effects and its history; parking does not reduce
  stored data.

## Alternatives rejected

- **New `AgentPhase::AwaitingApproval`.** Cleaner on paper, but it changes the
  legal phase graph, the validator's event/phase table, and every Stepper's
  expectations, for a state that is already representable as
  `AwaitingTool` + `AwaitingApproval` Effects.
- **`RunError::AwaitingApproval`.** Treats a normal wait as a failure and forces
  every caller to special-case an error path.
- **A separate `RunConclusion` return type.** Would change `Agent::run`,
  `Session::run_turn` and every call site at once; extending `RunStatus` keeps
  the signature.
- **Parking without a persisted decision store.** The Run would need to be
  re-asked from scratch, losing the operator's answer across restarts.

## Test plan

1. A deferred approver parks the Run: `status() == AwaitingApproval`, Effect is
   `AwaitingApproval`, lease released.
2. Writing an approval decision and resuming continues the Run and commits the
   Effect.
3. Writing a rejection and resuming produces a rejected Effect and a failed
   tool outcome, matching the inline path.
4. A parked Run with no decision is `skipped` by `recover_once`, not re-parked.
5. An expired or policy-mismatched decision re-parks instead of authorizing.
6. Interactive approvers (TUI, `--yes`) still complete inline with no parking.
7. Pooled and inline modes park identically.

## Open questions

- Should parking be bounded by a deadline that turns it into a rejection?
- Should a parked Run expose a distinct `RunEvent` so subscribers see the wait?
- Should `Session` record a parked turn, or leave the turn open?

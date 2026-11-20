# Child Runs Share Budget, Cancellation and Optional Execution

**Status:** accepted; the budget reservation described below is superseded by ADR-0054

Child Runs had identity and cancellation, but `spawn_subagent` did not charge
the parent budget, children could not share the parent's journal or approver,
child events were invisible, and `FailFast` dropped unjoined children.

Child execution now has four explicit sharing mechanisms:

- **Budget** — superseded by ADR-0054: child budgets are independent and
  capped, and the parent is not charged. The original text follows. The engine
  creates a `BudgetHandle` for each tool batch.
  Tools receive it through `ToolContext`; `spawn_subagent` reserves four turns
  and four tool calls from the parent before starting the child. The engine
  writes the updated snapshot back to Run state after the batch. The whole
  child allowance is charged up front and unused budget is not returned.
- **Cancellation** — a child flag remains a child of the parent flag, and the
  handle can cancel one child without stopping the parent.
- **Execution inheritance** — `ChildExecutionContext` can pass the parent's
  `DurableTransitionStore` and `Approver` into a child engine. A shared journal
  records child effects under the child Run id; a shared approver lets a child
  use approval-required tools.
- **Events** — `ChildSpec::forward_events` exposes a `ChildHandle` event stream.
  `FailFast` now cancels and awaits remaining children instead of dropping
  handles.

`spawn_subagent` uses the parent context for budget, cancel and execution
inheritance. A child with a shared journal exposes `parent_run_id` in its
stored state and can be resumed through `Agent::resume` while its Agent keeps
the same journal.

Consequences: a parent cannot be over-committed by subagents, and a headless
child can share approval and persistence configuration. Budget is reserved
per spawn, not settled against actual usage; a child that uses less keeps the
difference consumed. Parent recovery does not reconcile child runs, and
`spawn_subagent` is `NonIdempotent`, so a spawn interrupted in flight remains
an unknown outcome. Event forwarding is opt-in to avoid buffering long runs.

Alternatives considered: letting tools mutate `AgentState` directly (breaks
the state boundary), charging the child after completion (allows
over-commitment under concurrency), and sharing the parent engine wholesale
(the child would inherit the parent's tool registry, defeating restricted
subagents).

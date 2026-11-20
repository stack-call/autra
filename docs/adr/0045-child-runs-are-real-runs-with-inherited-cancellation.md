# Child Runs Are Real Runs with Inherited Cancellation

**Status:** accepted

`spawn_subagent` called `Agent::run` inside a tool. The nested Agent had no
parent identity, no shared cancellation, no join policy and no way for the
runtime to treat it as part of the parent's structured concurrency. The
`ExecutionScope` type existed but was not connected to execution.

A child is now a real Run on `LocalEngine`:

- `ChildSpec` carries the stepper, input, history, budget, `parent_run_id` and
  the parent cancellation flag;
- `LocalEngine::spawn_child` assigns a fresh `RunId`, sets
  `AgentState.parent_run_id`, and runs the child in a Tokio task;
- `ChildHandle` exposes `run_id`, `cancel` and `join`;
- `ChildRunSet` joins with `JoinPolicy::WaitAll` or `FailFast`;
- `CancelFlag::child_of` gives the child an inherited flag: parent cancellation
  reaches the child, child cancellation does not reach the parent;
- `SubagentTool` calls `Agent::run_as_child`, so a subagent is a ChildRun.

The engine passes a richer `ToolContext` (current Run id and cancellation flag)
through `Dispatcher::call_tool_with_context`; the default method keeps
dispatchers that only implement `call_tool` working.

Consequences: a parent can cancel and join children without knowing how they
are executed, and a child's effects are recorded under its own Run id with
`parent_run_id` in its state. `BudgetSnapshot::reserve_child` reserves a parent
turn and the child's tool-call allowance up front, but `spawn_subagent` does
not reserve automatically yet; the child uses its own Agent budget. The child
uses its own engine, so sharing a journal or approver requires building the
child Agent with the same configuration. Parent events are not forwarded to
the parent callback. `FailFast` cancels remaining children but does not await
them. These are deliberate v1 limits, not guarantees.

Alternatives considered: keeping tool-backed nested Agents (no structured
concurrency), making `spawn_subagent` share the parent engine (the child would
inherit the parent's tools, defeating restricted subagents), and adding a
global child registry (unnecessary before durable child discovery is needed).

# Structured Concurrency

Structured concurrency is a framework feature, not a distributed scheduler.

## Four levels

| Level | Supported now | Core? |
|---|---|---|
| Parallel tools within one Run | Bounded parallel batches, default 1 | Yes |
| ChildRun / Subagent | Tool-backed restricted subagents; runtime ChildRun pending | Yes |
| Multiple local Sessions | Independent Agents; no scheduler | Basic |
| Actor / WorkerPool / Kubernetes | Not yet; separate future component | No |

## What is implemented

- A model response may request multiple tool calls. With the default
  `max_parallel_tool_calls = 1`, the coordinator executes them sequentially in
  model order.
- With a larger limit, calls execute in chunks of at most that many concurrent
  tasks. Every outcome is recorded before the next model call.
- Approvals are resolved sequentially before a batch executes; a rejected call
  does not run and becomes an error outcome.
- `ToolCallStarted` is emitted when a call is dispatched. `ToolResult` stream
  events follow actual completion order; accepted history and model feedback
  follow model call order.
- A tool task panic becomes an `is_error` outcome with `outcome_unknown = true`;
  sibling calls are not abandoned.
- `max_turns` counts model calls; `max_tool_calls` counts accepted tool calls.
  Defaults are 8 and 8. `max_parallel_tool_calls` defaults to 1 and can be
  overridden per Run.
- A batch that exceeds the remaining tool budget is rejected before any call in
  that batch runs.
- `CancelFlag` is checked before every Effect. An in-flight Effect is allowed
  to finish and be recorded; no new Effect starts.
- `effect_timeout` can bound a model or tool call. A model timeout fails the
  Run; a tool timeout is an error outcome marked `outcome_unknown`.
- No orphan tasks are created: v1 has no detached `tokio::spawn` for Effects.

## Rules for parallel tools

- Use a bounded fan-out; never one unbounded task per tool call.
- Feed results back in the model's call order, even if completion order
  differs.
- A durable host records authoritative outcomes in actual commit order. In
  LocalEphemeral, accepted history is model-ordered and stream events carry
  completion order.
- Treat shared Workspace writes as serial or version-checked. Parallel
  read-only calls are the safe default. See
  [`ADR-0015`](adr/0015-parallel-tool-batches-are-bounded-and-ordered.md).
- A cancellation check must happen before each dispatch, not once per batch.
- `max_parallel_tool_calls`, `max_child_runs`, `max_model_inflight` and
  `max_external_processes` are separate limits, not one global count.

## Subagents today

`spawn_subagent` runs a named child Agent with its own model, tools, approval
configuration, context and budgets. The child Run is fresh and returns its
answer as a ToolOutcome. There is no parent cancellation propagation, shared
budget slice or Join policy yet; the parent cannot end a child, and a child
that lacks its own budget limits can run to its configured maximum.

## ChildRun requirements

A ChildRun must declare:

- `parent_run_id` and a scope that is a subset of the parent's permissions;
- an explicit budget slice;
- a Join policy (`FailFast`, `CollectAll`, `BestEffort`);
- a workspace policy;
- a cancellation path back to the parent.

A parent cannot finish while a child is silently orphaned. See the spec §10.2.

## Join policies

| Policy | Behaviour |
|---|---|
| `CollectAll` | Wait for every child and merge all results |
| `FailFast` | On failure, request cancellation of the rest and clean up |
| `BestEffort` | Keep successful results and preserve failure information |
| `FirstSuccess` | Accept the first success and cancel work no longer needed |

Separate semantic completion, resource reclamation and external side-effect stop.
A cancellation request does not prove that an external action stopped. Runtime ChildRun
and these policies are not implemented yet.

## What this is not

This is not a replacement for Tokio scheduling, Kubernetes, or a distributed
actor placement system. Remote workers need their own ownership, lease and
fencing design before they can be added.

## ChildRun

A child Run is a real Run on the local engine, not a nested `Agent::run`:

- it has its own `RunId` and `AgentState.parent_run_id`;
- it starts from a `ChildSpec` with its own stepper, input, history and
  `BudgetSnapshot`;
- its cancellation flag is a child of the parent flag: cancelling the parent
  cancels the child, but cancelling a child leaves the parent running;
- `ChildHandle` exposes `run_id`, `cancel` and `join`;
- `ChildRunSet` joins children with `JoinPolicy::WaitAll` (default) or
  `JoinPolicy::FailFast`, which cancels remaining children after the first
  error.

`spawn_subagent` enters through `Agent::run_as_child`, so the child has parent
identity and inherited cancellation. The child runs on its own Agent's engine;
`ChildExecutionContext` can pass the parent's journal and approver into that
engine. A shared journal records child effects under the child Run id and
exposes `parent_run_id` in the stored state, so the child can be resumed.

The engine creates a `BudgetHandle` per tool batch and passes it through
`ToolContext`. A child Run has its own budget: `spawn_subagent` gives it four
turns and up to four tool calls, capped by the parent's remaining tool calls
without charging the parent. The spawn itself is one parent tool call. A
parent with no remaining tool calls can still spawn a child, but the child gets
a zero-tool allowance. `BudgetHandle::reserve_child` remains available for
explicit budget slicing but is not used by the default subagent tool.

`ChildSpec::forward_events` exposes child Run events through
`ChildHandle::events`/`take_events`. `FailFast` cancels and awaits remaining
children. Parent recovery still does not reconcile child runs; an interrupted
spawn remains an unknown outcome because `spawn_subagent` is `NonIdempotent`.

Limits: the reservation is fixed (4/4) rather than configurable per subagent;
the parent's `RunOptions` are not inherited; event forwarding is opt-in.

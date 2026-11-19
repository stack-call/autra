# Structured Concurrency

Structured concurrency is a framework feature, not a distributed scheduler.

## Four levels

| Level | Supported now | Core? |
|---|---|---|
| Parallel tools within one Run | Bounded parallel batches, default 1 | Yes |
| ChildRun / Subagent | Not yet | Yes |
| Multiple local Sessions | Independent Agents; no scheduler | Basic |
| Actor / WorkerPool / Kubernetes | Not yet; separate future component | No |

## What is implemented

- A model response may request multiple tool calls. With the default
  `max_parallel_tool_calls = 1`, the coordinator executes them sequentially in
  model order.
- With a larger limit, calls execute in chunks of at most that many concurrent
  tasks. Every outcome is recorded before the next model call.
- `ToolCallStarted` is emitted when a call is dispatched; outcomes are fed back
  and recorded in model order even when completion order differs.
- A tool task panic fails the Run as a protocol error.
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
- Record authoritative outcomes in actual commit order, not model order.
- Treat shared Workspace writes as serial or version-checked. Parallel
  read-only calls are the safe default. See
  [`ADR-0015`](adr/0015-parallel-tool-batches-are-bounded-and-ordered.md).
- A cancellation check must happen before each dispatch, not once per batch.
- `max_parallel_tool_calls`, `max_child_runs`, `max_model_inflight` and
  `max_external_processes` are separate limits, not one global count.

## ChildRun requirements

A ChildRun must declare:

- `parent_run_id` and a scope that is a subset of the parent's permissions;
- an explicit budget slice;
- a Join policy (`FailFast`, `CollectAll`, `BestEffort`);
- a workspace policy;
- a cancellation path back to the parent.

A parent cannot finish while a child is silently orphaned. See the spec §10.2.

## What this is not

This is not a replacement for Tokio scheduling, Kubernetes, or a distributed
actor placement system. Remote workers need their own ownership, lease and
fencing design before they can be added.

# Bounded parallel tool calls

**Status:** ready-for-agent

**Source:** design spec §10.3 (parallel tool fan-out/join) and §10.4 (bounded
concurrency).

## Problem Statement

A model can request several independent tool calls in one response. The
runtime currently executes them sequentially even when they are read-only and
independent, so latency is the sum of every call.

## Solution

Add an opt-in `max_parallel_tool_calls` limit. When it is greater than one, the
runtime executes a tool batch with bounded fan-out, waits for every dispatched
call, and feeds all outcomes back to the model in the original call order. The
default stays 1, so existing behaviour and shared-workspace safety are
unchanged.

## Implementation Decisions

- `AgentBuilder::max_parallel_tool_calls(u32)` sets the default for an Agent;
  `RunOptions::max_parallel_tool_calls: Option<u32>` overrides it per Run.
  Effective limit is at least 1.
- A batch of `CallTool` intents is split into chunks of at most the effective
  limit. Each chunk is dispatched with a `tokio::task::JoinSet`; the runtime
  awaits the whole chunk before starting the next one.
- The sequential path remains for limit 1 or a single call.
- `ToolCallStarted` is emitted when a call is actually dispatched. A
  cancellation check runs before each dispatch; calls already dispatched are
  awaited and recorded.
- `ToolOutcome`s are fed back in the model call order, regardless of
  completion order. History and `ModelResult` feedback are therefore
  deterministic. `ToolResult` stream events follow actual completion order.
- A tool task panic becomes an `is_error` outcome with `outcome_unknown = true`;
  sibling calls continue and are recorded.
- Parallelism does not imply safe shared writes. Built-in tools remain
  read-only; the default limit stays 1 until write tools have version checks.
- No ChildRun, worktrees or distributed workers in this slice.

## Testing Decisions

- Use a test tool that records active count and maximum concurrency.
- Assert bounded concurrency (limit 2 with two calls reaches 2; limit 1 never
  exceeds 1).
- Assert history tool order follows model call order when a later call finishes
  first.
- Assert a RunOptions limit overrides the builder limit.
- Assert cancellation before a batch dispatches no tools, and cancellation
  during a batch records in-flight outcomes without starting later chunks.
- Assert `ToolResult` stream events follow completion order while accepted
  history follows model order.
- Assert a panicking tool becomes an unknown error outcome in both sequential
  and parallel modes.
- Keep timeout behaviour covered by the existing bounded-run tests.

## Out of Scope

ChildRun/Subagents, Workspace worktrees, write-tool conflict resolution,
distributed scheduling, and any change to Stepper semantics.

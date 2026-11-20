# Restricted subagents

**Status:** ready-for-agent

**Source:** design spec §7.2 (Subagents), §10.2 (ChildRun scope) and M2/M5.

## Problem Statement

A parent Agent has no way to delegate a self-contained task to a narrower Agent
with its own model, tools and budget. Everything runs in one tool set and one
context.

## Solution

Add a tool-backed subagent. A parent registers named child Agents with their own
definition, tools and budgets; the model calls `spawn_subagent` with a name and
task. The child runs as an independent Agent Run and returns its answer as a
ToolOutcome.

## Implementation Decisions

- `SubagentRegistry` maps a name and description to an `Arc<Agent>`.
  `AgentBuilder::subagent(name, description, child)` registers one.
- A single `spawn_subagent` tool exposes the registry. Its schema enumerates the
  available names.
- The child Run is fresh: no parent history, its own model, tools, approvals,
  context and budgets. The parent does not inherit the child's state.
- The child's permissions are exactly its own tool set. Registering a subagent
  does not widen the parent or child permissions.
- The tool declares a non-idempotent effect class. A child error becomes an
  `is_error` ToolOutcome; the parent model can react.
- This is not a runtime ChildRun: there is no parent cancellation propagation,
  shared budget slice or Join policy. Those require a `SpawnChild` Effect and a
  future runtime slice. Recursion is possible only if the user explicitly gives
  a child the same subagent capability.

## Testing Decisions

- Runtime: parent spawns a named child; child answer becomes a tool result;
  parent completes.
- Unknown subagent name becomes an error outcome.
- A child Run failure becomes an error outcome and the parent can continue.
- The tool schema lists registered names.
- Existing parallel, approval, session and context tests stay green.

## Out of Scope

Runtime ChildRun identity, cancellation propagation, budget slicing, join
policies, recursive depth limits and cross-process subagents.

# Planning tool

**Status:** ready-for-agent

**Source:** design spec §7.2 (Planning) and M5.

## Problem Statement

The harness has no explicit plan. A model doing multi-step work can only keep
intentions in prose, and the caller cannot inspect what the Agent intends to do.

## Solution

Add an `update_plan` tool backed by an in-memory plan owned by the Agent.
`AgentBuilder::planning()` registers the tool and appends a short planning
instruction to the system prompt. `Agent::plan()` exposes the current items.

## Implementation Decisions

- `PlanItem` has `title` and `status` (`pending`, `in_progress`, `completed`).
- `update_plan` replaces the whole plan and returns a rendered checklist. It is
  read-only and in-memory; no approval is required.
- The plan is shared per Agent through a mutex, so it survives turns and is
  visible to the caller. Concurrent runs on one Agent share the same plan.
- `planning()` also adds the instruction: keep a short plan and update it with
  `update_plan`.
- No Planner engine, automatic decomposition or plan persistence.

## Testing Decisions

- Harness: replace/render statuses, empty plan, invalid status.
- Runtime: model calls `update_plan` then answers; `Agent::plan()` reflects the
  items; the system prompt mentions `update_plan`.
- Existing tests stay green.

## Out of Scope

Planner engines, plan graphs, persistent plans, subagent plan merging and UI.

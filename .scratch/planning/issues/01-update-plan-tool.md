# 01: update_plan tool

**What to build:** An Agent can maintain an explicit todo list that the model updates through a tool, and the caller can read the resulting plan.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `PlanItem` and `PlanStatus` model pending, in-progress and completed items.
- [x] `update_plan` replaces the plan and returns a rendered checklist.
- [x] `AgentBuilder::planning()` registers the tool and adds the planning instruction.
- [x] `Agent::plan()` exposes the current plan items.
- [x] Invalid statuses and malformed arguments are tool errors.
- [x] Harness and runtime tests cover rendering, statuses and end-to-end use.
- [x] Docs and an ADR record the tool-backed, in-memory planning boundary.

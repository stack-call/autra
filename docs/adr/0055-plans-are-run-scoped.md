# Plans Are Run-Scoped

**Status:** accepted. Supersedes the per-Agent plan scope in ADR-0018.

## Context

`update_plan` originally stored one `Arc<Mutex<Vec<PlanItem>>>` on the Agent.
`Agent::plan()` exposed it. That made the plan an Agent-level shared object:
two concurrent Runs on the same Agent could overwrite each other's checklist,
and a resumed Run could not tell which plan belonged to it.

A plan is execution state for one Run, not a capability of the Agent. Tools,
providers and policies are Agent-scoped because they are immutable
configuration; a checklist is not.

## Decision

- `PlanStore` maps `RunId -> Vec<PlanItem>`.
- `UpdatePlanTool::execute` writes under `ToolContext::run_id`.
- `Agent::plan_for(&RunId)` reads one Run's plan.
- `SharedPlan` remains as a compatibility alias for `PlanStore`, but it no
  longer names an Agent-wide value.

The in-memory store is still not durable. Persisting a plan requires an explicit
Run/Session state protocol; a mutex-backed map is not a recovery mechanism.

## Consequences

- Concurrent Runs on one Agent cannot overwrite each other's plans.
- Callers must supply a Run id to read a plan.
- `update_plan` requires an execution context; calling the tool directly returns
  an error.
- Resume can distinguish plans by Run, but plan persistence remains future work.

## Alternatives considered

- Keep an Agent-level plan and clone it per Run: still ambiguous after resume.
- Store the plan in `EngineState`: workable for ReAct but would force every
  engine to understand planning. A Run-keyed store keeps it at the Harness
  capability layer.

See ADR-0018 for the original tool-backed planning decision.

# Planning Is Tool-Backed and In-Memory

**Status:** accepted; plan scope superseded by ADR-0055

A planning abstraction could grow into a Planner engine with automatic
decomposition. That would add a second decision layer beside the Stepper before
a real need exists.

We decided planning is a tool the model calls. `update_plan` replaces a short
checklist with pending, in-progress and completed items and returns the rendered
plan. `AgentBuilder::planning()` registers it and adds a short instruction;
`Agent::plan_for(&RunId)` exposes the items. The plan is per Run (see
ADR-0055), mutex-protected and
in-memory.

Consequences: the model owns the plan shape, and concurrent Runs on one Agent
share it. There is no automatic decomposition, persistence or merging with a
subagent plan.

Alternatives considered: a Planner engine (premature), plan in Context only
(invisible to the caller), and a persistent plan store (durable-host work).
Revisit when evaluation shows the tool-backed plan is insufficient.

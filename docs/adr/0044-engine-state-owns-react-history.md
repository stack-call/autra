# Engine State Owns ReAct History

**Status:** accepted

`AgentState` still carried `history` and `pending_tool_calls`, and its default
engine state was `EngineState::new("react", 1)`. A second engine would either
inherit ReAct fields it does not use or have to work around them, so the
"engine-agnostic Core" claim held only at the type level.

Core now stores only identity, phase, state version, budget, final answer,
last error and an opaque `engine_state`. ReAct serializes a `ReactState`
(history and pending tool calls) into `engine_state` with
`engine_id = "react"` and schema revision 1. The harness exposes
`react_history`, `react_pending_tool_calls` and `react_engine_state` for the
facade, sessions, resume fixtures and tests.

The `AgentStepper` contract gained two hooks:

- `initialize(&mut AgentState, &[HistoryItem])`, called once for a new Run to
  seed engine state (Session history seeding);
- `pending_tool_calls(&AgentState)`, used by `LocalEngine` when reconciling an
  `AwaitingTool` checkpoint without reading engine-private fields.

Consequences: Core no longer names ReAct or carries its fields; a custom
Stepper owns its data and can ignore both hooks (defaults are empty). The
facade and `Session` read history through the ReAct helper rather than
`AgentState`. Costs: engine state is JSON, so schema changes need an explicit
revision and migration policy, and malformed or foreign state falls back to an
empty ReAct state. Removing the old `history` access is a breaking change.

Alternatives considered: keeping the fields as deprecated (Core stays
ReAct-shaped), storing history in a separate side table (more moving parts for
no semantic gain), and giving `AgentStepper` a full `init_state` returning a
new `AgentState` (it would let an engine overwrite identity and budget).

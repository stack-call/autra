# RunState Separates Core and Engine State

**Status:** accepted

A custom Engine could not safely extend `AgentState` without changing Core, and
Core fields such as run id, phase and budget were mixed with ReAct-specific
history and pending tool calls.

We added `EngineState { engine_id, schema_revision, data }` to the Run state.
Core owns identity, phase, budget, answer and error; an Engine may ignore the
ReAct fields and keep its own serialized state in `engine_state`. Core stores
and versions it opaquely.

Consequences: a custom Engine can add state without editing Core. The current
ReAct fields remain for compatibility; migration, protocol negotiation and
cross-engine state conversion are still future work. An Engine cannot use
`engine_state` to bypass accepted events or host authority.

Alternatives considered: a generic `Any` field (not serializable), a fully
generic `RunState<T>` (infects every API), and separate state tables per engine
(premature). Revisit when a second real engine is implemented.

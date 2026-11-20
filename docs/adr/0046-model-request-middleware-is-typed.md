# Model Request Middleware Is Typed

**Status:** accepted

The extension design calls for behaviour extensions that participate before an
Effect is accepted, without letting them mutate authoritative Run state. The
harness had no such seam: changing the model request meant replacing the
Stepper or the ContextBuilder wholesale.

`autra-harness` now provides:

- `ModelBuildContext` with read-only `run_id` and `turn`;
- `ModelRequestDraft` with only `system`, `messages` and `tools`;
- `ModelRequestTransform` for one ordered transform;
- `ModelMiddleware`, a chain applied in registration order;
- `MiddlewareStepper`, which wraps any `AgentStepper` and rewrites each
  `CallModel` intent before the coordinator records it.

`AgentBuilder::middleware` registers transforms. Without middleware the request
is unchanged. A transform error becomes a `StepError`, so the Run fails before
a model Effect is recorded. The draft cannot reach `AgentState`, the journal or
a committed Effect, and the transform runs in the same path regardless of
whether the engine is ReAct or custom.

Consequences: logging, redaction, prompt fragments, tool visibility and model
routing can be composed without touching Runtime. Middleware still cannot
perform hidden external I/O; a transform that needs retrieval must produce a
Context View through a controlled capability first. Wrap middleware around the
real invocation (cache, retry, transport metrics) is not implemented yet; that
belongs to the Execution Host and must follow Effect attempts and recovery.

Alternatives considered: a stringly-typed event bus (unrestricted mutation), a
`ContextBuilder` wrapper (only covers ReAct's default path), and a Runtime-level
interceptor (too late: the Effect would already be recorded).

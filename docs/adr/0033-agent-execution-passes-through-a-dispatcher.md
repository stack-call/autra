# Agent Execution Passes Through a Dispatcher

**Status:** accepted

The coordinator called the model provider and tool registry directly, so a
sandbox, remote worker or test double could not intercept effect execution
without editing the runtime.

We added a `Dispatcher` trait with `call_model` and `call_tool`, a
`LocalDispatcher` wrapping the provider and tool registry, and
`AgentBuilder::dispatcher`. A custom dispatcher can replace the local executor;
a custom dispatcher also removes the need for a builder model.

Consequences: execution has one replaceable boundary. A journaled Agent now
enqueues and claims effects through the outbox loop; see ADR-0035. At-least-once
delivery is still not promised.

Alternatives considered: direct calls (current status quo), a trait on the model
only (tools still bypass), and a separate process dispatcher (premature).
Revisit with the claim-driven dispatcher loop.

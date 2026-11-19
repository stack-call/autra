# Philosophy

Autra is a Rust-native agent framework: composable intelligence, structured
execution, explicit effects, optional infrastructure.

The full design proposal is
[`../agent_native_framework_complete_spec_v1.0.md`](../agent_native_framework_complete_spec_v1.0.md).
This page is the short version used when making decisions.

## Positioning

**Framework first; infrastructure when needed.** A developer can build and run
an Agent in one process, with no database, server, sandbox or Kubernetes. The
same Agent definition can later opt into durability, sandboxing or a hosted
execution backend, but those are never a prerequisite.

Autra is not an Agent hosting platform. It does not implement a general
Durable Workflow engine, an OS sandbox, a task scheduler or a distributed
actor placement system.

## Nine principles

| Principle | Constraint |
|---|---|
| P1 Small Core, Rich Harness | Core defines State, Decision, Effect and execution identity; Planning, Skills and MCP live above it |
| P2 Behavior Composable, Semantics Protected | Extensions can change models, tools and context, but not legal state transitions |
| P3 Structured Agent Concurrency | Child Runs and parallel tools have a parent scope, budget, cancellation and Join |
| P4 Explicit Effects | Model calls and external tools are traceable I/O boundaries |
| P5 Context Is a Versioned View | Context is what the model sees, not the authoritative journal |
| P6 Durability Is Conditional | Only an explicit host or durable boundary promises recovery |
| P7 Authority Is External to the Agent | Tools and Skills cannot grant OS, network or credential authority themselves |
| P8 Logical Work Outlives Process | Session and Run identities are separate from a Tokio task or worker |
| P9 Evidence Over Claims | Trace, contract tests and evaluation accompany every reliability claim |

## What not to claim

- A successfully dispatched tool is not proof that the external action was
  undone by cancellation. See INV-06.
- A serialized state is not automatically transactional durable execution.
- A local shell is not a sandbox.
- A Restate host and a local host do not provide identical guarantees. See
  INV-08.
- A token stream or OpenTelemetry trace is not a recovery journal.

## Failure conditions for the design

If a simple Agent needs more boilerplate than a direct provider wrapper, the
high-level API is not simple enough. If concurrent tools produce
unexplainable state races, structured concurrency has failed. If recovery
silently repeats an external action, the execution contract is insufficient.
If running one Agent requires a platform, the product boundary has drifted.

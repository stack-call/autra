# The Product Facade Assembles Harness and Runtime

**Status:** accepted

`Agent`, `AgentBuilder`, `Session`, `Subagent`, the worker loop and the local
engine all lived in `autra-runtime`. Harness depended on core, runtime
depended on harness, and a custom engine still had to assemble harness types
inside runtime. The dependency direction forced behaviour and infrastructure
into one layer.

The facade is now a separate `autra` crate:

- `autra` owns `Agent`, `AgentBuilder`, `Session` and `Subagent`; it builds a
  `ReActStepper` (or a custom one), assembles tools and calls `LocalEngine`.
- `autra-runtime` owns `LocalEngine`: the coordinator, transition commit,
  dispatch, outbox, resume and reconciliation. It depends on core and execution
  only; its `Cargo.toml` no longer depends on harness.
- `autra-harness` owns behaviour and concrete tools. `run_command` and the git
  tools moved here behind an optional `process` feature, because they need
  Tokio and a workspace.
- The public run API (`RunEvent`, `RunOptions`, `RunOutcome`, `RunError`,
  `RunStatus`) moved to `autra-execution::run`, so runtime can return it
  without depending on the facade.
- The CLI and `autra-eval` depend on `autra`, not on runtime. Durable adapters
  depend on core and execution only.

`autra-testkit/tests/architecture.rs` reads `cargo metadata` and enforces the
normal dependency rules: core has no Autra dependencies; execution depends on
core; harness and runtime depend only on core and execution; durable adapters
do the same; the facade assembles them; CLI/eval do not import runtime.

Consequences: a new engine can be assembled through `AgentBuilder::stepper`
without editing runtime; a new host can implement execution contracts without
touching harness; the facade can grow product API without changing execution
semantics. `LocalEngine` is generic over `AgentStepper` and dispatches through
the `Dispatcher` contract. Costs: one more crate and one more layer of
re-exports; existing `autra_runtime::Agent` imports must switch to
`autra::Agent` (a deliberate breaking change). Tests for the facade moved to
the `autra` crate.

Alternatives considered: keeping `Agent` in runtime and re-exporting it from
the facade (the dependency direction would stay wrong), leaving `RunEvent` in
runtime and having the facade pass callbacks (runtime would still define the
product API), and moving process tools to a separate crate (more ceremony for
two tools; the feature flag keeps harness Tokio-free by default).

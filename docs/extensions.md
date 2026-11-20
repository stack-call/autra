# Extension Architecture

Today Autra extends through Rust traits, explicit Builder composition and
optional adapters. It is not yet a dynamic plugin platform; the intended
extension system is described at the end of this page as a design-only
target.

## Plugins are a future layer

A capability (Tool, Provider, Context strategy, Policy, UI panel) is different
from a plugin, which bundles several capabilities, validates dependencies and
owns a lifecycle. The proposed plugin structure is recorded in
[`plugins.md`](plugins.md) as a design-only document; no plugin API exists yet.
Direct Builder registration remains the supported extension path.

## Three levels

| Level | Audience | Examples | Rule |
|---|---|---|---|
| Capability | Agent developer | Tool, Model Provider, Context strategy, Skills, Memory, MCP | Register through the Builder; do not edit Core or Runtime |
| Behavior | Advanced developer | Engine, Planner, Subagent policy, Hooks, strategies | Reuse the execution protocol; do not bypass scope or approval |
| Infrastructure | Framework/platform developer | Durable Host, Sandbox, Gateway, Telemetry, Workspace, Remote Worker | Optional adapter; do not pollute Core |

## Extension groups

| Group | Interfaces | Current status |
|---|---|---|
| Intelligence | `AgentStepper`, `ContextBuilder`, Planner, Memory, `SkillRegistry` | Engine, Context and Skills implemented; Planner and Memory pending |
| Capability | `Tool`, `ModelProvider`, MCP | Tool and Provider implemented; MCP pending |
| Execution | Execution Host, `Dispatcher`, `SandboxBackend`, Workspace | Host/Sandbox and Dispatcher contracts defined; LocalDispatcher, SQLite journal and a claim-driven outbox loop used by journaled Agent runs |
| Observation | Hooks, `tracing`, `autra-eval` | tracing, evaluation and `ObserverHook` implemented; Behavior/Policy hooks pending |

## Current extension points

| Extension | Registration | Contract today |
|---|---|---|
| Tool | `AgentBuilder::tool` | `descriptor`, `contract`, `execute(input, context)`; `spec`/`call` remain as default adapters. The coordinator owns approval and dispatch |
| Model provider | `AgentBuilder::model` | `ModelProvider::complete`, `capabilities()` and `protocol_version()`; provider-specific fields stay in the adapter |
| Context strategy | `AgentBuilder::context_builder` | `ContextBuilder::build`; a custom builder owns skills and budget composition |
| Engine | `AgentBuilder::stepper` | `AgentStepper::transition` returns a proposed transition; the coordinator commits and dispatches |
| Model request middleware | `AgentBuilder::middleware` | `ModelRequestTransform` sees a restricted `ModelRequestDraft`, never `AgentState`; transforms run in registration order before the Effect is recorded |
| Skill | `AgentBuilder::skill` | Named instructions plus read-only `load_skill`; loading grants no permission |
| Subagent | `AgentBuilder::subagent` | Named child Agent behind `spawn_subagent`; runs as a runtime ChildRun with parent identity and inherited cancellation |
| Approver | `AgentBuilder::approver` | `Approver::approve`; the coordinator binds the decision to the call |
| Dispatcher | `AgentBuilder::dispatcher` | `call_model` and `call_tool`; custom model/tool executor |
| Observation | `AgentBuilder::observer`, `tracing` subscriber, `autra-eval` | `ObserverHook` sees run/model/effect/finish stages; traces and evaluation are not execution state |

A new Tool, Provider, Context strategy or Stepper does not require editing
`autra-core` or `autra-runtime` internals.

## Extension contracts

Every extension group should declare:

- **Interface**: the trait and the data it may read or return.
- **Registration**: the Builder path or adapter constructor.
- **Dependencies**: what must exist first, and what must be closed last.
- **Lifecycle**: initialization, readiness, cancellation, close and error handling.
- **Security**: what it may not do; which authority remains in the trusted host.
- **Contract tests**: a deterministic test proving the extension obeys the protocol.

## Principles

- **Extension by Composition**: compose existing components rather than modify
  Core or inherit a large base class.
- **Semantics Are Protected**: an extension may change behaviour, never bypass
  scope, approval, budget or commit.
- **Capabilities Are Explicit**: an extension declares what it provides; a host
  declares what it guarantees; unsupported operations return typed errors.
- **Lifecycle Is Managed**: dependencies, initialization and shutdown have a
  defined order.
- **Progressive Extensibility**: a Tool starts with a small contract; only an
  Engine or Host needs the deeper protocols.

## Not a plugin platform

- No dynamic loading, hot swap, marketplace or global service locator in v1.
- Rust trait extensions run in the host process with the host permissions. A
  Trait is not a sandbox.
- Untrusted extension code belongs in a Tool Host or Sandbox with its own
  authority boundary.

## Current gaps

| Gap | Consequence |
|---|---|
| Tool protocol version negotiation | Extensions cannot yet declare compatibility with stored Runs |
| `ModelCapabilities` | No declared streaming/structured-output/multimodal support |
| Planner trait | Planning is currently a tool, not a replaceable behaviour |
| Sandbox adapter and lifecycle binding | The trait exists, but no backend is wired into execution |
| Dependency and close ordering | Builder composition has no explicit lifecycle manager |
| Protocol version negotiation | Extensions cannot yet declare compatibility with stored Runs |
|| Cross-Run compatibility negotiation | Protocol versions exist, but stored Run migration is not negotiated |
| Streaming and structured-output adapters | Capabilities are declared; the OpenAI adapter does not implement them |
| Planner trait | Planning is currently a tool, not a replaceable behaviour |
| Sandbox adapter and lifecycle binding | The trait exists, but no backend is wired into execution |
| Dependency and close ordering | Builder composition has no explicit lifecycle manager |
| Per-adapter behavioural tests | `autra-testkit` checks protocol basics; host/sandbox behaviour needs its own suite |

## Future: the full extension system (design only)

The complete proposal — four layers, middleware split, plugin lifecycle,
scopes, permission rules and delivery order — is in
[`extension-architecture-v1.md`](extension-architecture-v1.md). The summary
below is the short version.

The current implementation covers capability registration through Builders and
a read-only `ObserverHook`. The intended extension system has four layers.
Capability registration, a subset of lifecycle events and typed model-request
transforms (`AgentBuilder::middleware`) exist today. Invocation wrapping and
plugin composition are not implemented. This section records the target shape
so new features do not invent a conflicting one.

```text
Autra Extension System
        │
        ├── Capability Registration
        │     Tool / Model / Context / Planner
        │
        ├── Lifecycle Events
        │     before_model / after_model / ...
        │
        ├── Wrap Interceptors
        │     model_call / tool_call ...
        │
        └── Plugin Composition
              dependencies / init / cleanup / UI
```

| Layer | Responsibility | Today | Future |
|---|---|---|---|
| Capability Registration | Declare Tools, Model Providers, Context strategies, Planners, Skills, Memory sources | Builder paths (`tool`, `model`, `context_builder`, `stepper`, `skill`) | Typed registries that a plugin or application can fill; duplicate and conflict checks |
| Lifecycle Events | Read-only notifications at defined stages | `ObserverHook` (run start, model request, effect accepted, run finished) | A typed catalog: `before_model`, `after_model`, `before_tool`, `after_tool`, approval, effect committed, run phase changes |
| Wrap Interceptors | Middleware around a call: inspect, transform or short-circuit | Behaviour wrap for model requests (`ModelRequestTransform`); no invocation wrap | Ordered `model_call` / `tool_call` chains for tracing, redaction, caching, policy checks, retries |
| Plugin Composition | Bundle capabilities, hooks and interceptors with dependencies, init/cleanup and UI | None; design in [`plugins.md`](plugins.md) | A `Plugin` / `PluginManager` in the facade (or `autra-plugins`) that installs the other three layers |

### Layer rules

- **Registration is not authorization.** Registering a Tool, Policy or UI panel
  does not grant it permission. Approval, scope, budget and commit remain in
  the trusted execution layer.
- **Lifecycle events are read-only.** An observer reports what happened; it
  cannot change the state, arguments or outcome. Behaviour changes belong in
  interceptors or engine components, subject to the same execution rules.
- **Interceptors wrap, they do not bypass.** A `model_call` or `tool_call`
  interceptor may transform a request or result, but may not skip approval,
  cancellation, budget checks or the journal. Ordering must be deterministic,
  and an interceptor must have a defined behaviour on error, timeout and
  cancellation.
- **Plugins compose, they do not execute.** A plugin fills registries and owns a
  lifecycle; it never becomes a second Run path or receives a raw host handle.
- **Layers are independently usable.** A single Tool can be registered without
  an observer; an observer can exist without a plugin; a plugin can install
  capabilities without UI. This is the progressive-complexity rule.

### Layer contract tests

Each layer needs its own contract test before it becomes a supported extension
point:

- registration: duplicate names, missing dependencies, rollback on failure;
- lifecycle events: ordering, read-only facts, no mutation of accepted state;
- interceptors: request/result transformation, ordering, cancellation and error
  propagation, no bypass of approval or scope;
- plugins: dependency graph, init/cleanup order, partial-install rollback.

See [`plugins.md`](plugins.md) for the plugin composition design.

# Extension Architecture

Autra extends through Rust traits, explicit Builder composition and optional
adapters. It is not a dynamic plugin platform.

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
| Execution | Execution Host, `SandboxBackend`, Workspace | Local is built in; Host/Sandbox traits pending |
| Observation | Hooks, `tracing`, `autra-eval` | tracing and evaluation implemented; typed Hooks pending |

## Current extension points

| Extension | Registration | Contract today |
|---|---|---|
| Tool | `AgentBuilder::tool` | `spec`, `effect_class`, `requires_approval`, `call(&str)`; the coordinator owns approval and dispatch |
| Model provider | `AgentBuilder::model` | `ModelProvider::complete`; provider-specific fields stay in the adapter |
| Context strategy | `AgentBuilder::context_builder` | `ContextBuilder::build`; a custom builder owns skills and budget composition |
| Engine | `AgentBuilder::stepper` | `AgentStepper::transition` returns a proposed transition; the coordinator commits and dispatches |
| Skill | `AgentBuilder::skill` | Named instructions plus read-only `load_skill`; loading grants no permission |
| Subagent | `AgentBuilder::subagent` | Named child Agent behind `spawn_subagent`; tool-backed, not runtime ChildRun |
| Approver | `AgentBuilder::approver` | `Approver::approve`; the coordinator binds the decision to the call |
| Observation | `tracing` subscriber, `autra-eval` | Traces and evaluation results are not execution state |

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
| ToolDescriptor/ToolContract split | `Tool` mixes description, effect class and execution; a richer contract is future |
| `ModelCapabilities` | No declared streaming/structured-output/multimodal support |
| Planner trait | Planning is currently a tool, not a replaceable behaviour |
| `SandboxBackend`/Host traits | Adapters cannot be registered yet |
| Dependency and close ordering | Builder composition has no explicit lifecycle manager |
| Protocol version negotiation | Extensions cannot yet declare compatibility with stored Runs |
| Shared contract-test suite | Each extension is tested ad hoc |

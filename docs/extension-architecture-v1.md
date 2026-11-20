# Autra Extension Architecture v1.0 (Design Proposal)

**Status: design proposal. None of the APIs below exist yet.** This document
records the intended extension architecture. Today Autra extends through Rust
traits and Builder composition, plus a read-only `ObserverHook`; see
[`extensions.md`](extensions.md) and [`plugins.md`](plugins.md).

## Core principle

> **Everything is extensible. Not everything must be a plugin.**

Autra adopts *Capability as the base, Middleware as the behaviour extension
mechanism, and Plugin as the composition and lifecycle unit.* It borrows service
dependencies, lifecycle and revocable registration from DSH/Cordis, and typed
lifecycle/wrap interception from LangChain, without making *Everything Is a
Plugin* a mandatory rule.

## 1. Overall shape

```text
Autra Application / Facade
  AgentBuilder · ApplicationBuilder · Plugin Composition
  Plugin Registry · Service Dependencies · Lifecycle Manager
                 │ registers capabilities and behaviour
                 ▼
autra-harness
  Agent behaviour · Context / Planning / Skills
  Lifecycle Hooks · Model Middleware
                 │
                 ▼
autra-runtime
  Coordinator / Scope · Effect Dispatch · Trusted Policy
                 │
                 ▼
autra-execution
  Tool · Model · Host · Sandbox · Run / Session Protocols
                 │
                 ▼
autra-core
  Stepper · State · Event · EffectIntent · ExecutionScope
```

The diagram shows responsibilities, not direct dependency cycles. Three rules
are non-negotiable:

1. **Core does not depend on the plugin system.** A normal plugin cannot
   rewrite state-transition semantics.
2. **Runtime does not manage plugins.** It executes capabilities that were
   registered, configured and authorized through legitimate contracts.
3. **Facade composes.** A simple Agent uses Builders; only a full application
   needs a `PluginManager`.

## 2. Four concepts, not one universal Extension

| Concept | Question it answers | Examples |
|---|---|---|
| **Capability** | What can the system do? | Tool, Model, Context, Sandbox |
| **Lifecycle Hook** | When does behaviour participate? | BeforeModel, RunCompleted |
| **Wrap Middleware** | How is an operation wrapped? | Model routing, caching, metrics |
| **Plugin** | How are capabilities organized, installed, depended on and managed? | GitPlugin, BrowserPlugin, WebUiPlugin |

- **Middleware = Lifecycle Hooks + Wrap Interceptors.**
- **Plugin = Capability Composition + Dependency / Resource Lifecycle.**

A plugin may register middleware, but middleware does not have to belong to a
plugin; a standalone Tool or ContextBuilder does not have to implement Plugin.
This absorbs the responsibility separation of DSH's event dispatch and
LangChain's model/tool middleware without copying either API wholesale.

## 3. Where each extension lives

Keep the current five layers; introducing plugins should not trigger a crate
reshuffle.

| Layer | Public extension point | Responsibility |
|---|---|---|
| `autra-core` | `AgentStepper`, base state protocol | Custom Agent engine |
| `autra-execution` | `Tool`, `ModelProvider`, `SandboxBackend`, host contracts | Executable capabilities |
| `autra-harness` | Context strategy, Planner, lifecycle, middleware | Change Agent behaviour |
| `autra-runtime` | Trusted host, policy, coordinator implementation interfaces | Enforce execution guarantees |
| `autra` | Builder, plugin registry, dependency resolution, lifecycle | Compose extensions |
| future `autra-ui` | Panel, renderer, command, view | Extend presentation |

Extension implementations live in separate Rust crates that depend only on the
public contracts they need:

```text
autra-plugin-git
  ├── Tool                  → autra-execution
  ├── Context middleware    → autra-harness
  └── Plugin manifest       → autra composition API
```

A separate crate does not imply a dynamic loader. v1 uses static linking and
Builder composition.

## 4. Capability: keep dedicated traits

Existing contracts stay as they are: `Tool`, `ModelProvider`, `ContextBuilder`,
`AgentStepper`, `SandboxBackend`. The common developer path stays:

```rust
let agent = Agent::builder("coding-agent")
    .model(model)
    .tool(ReadFileTool)
    .tool(GitStatusTool)
    .context(RepoContext)
    .build()?;
```

**A developer must not need a Plugin to register one Tool.** New planners,
providers, sandboxes and engines implement their dedicated contracts.
Middleware is for cross-cutting behaviour outside those capabilities. This
avoids `ContextBuilder` and `ContextMiddleware` competing for the same job.

## 5. Middleware: typed lifecycle and typed wrapping

Autra should use a **typed lifecycle + typed wrap**, not an unrestricted
stringly-typed event bus where any event can mutate anything.

### 5.1 Lifecycle hooks

| Hook | Behaviour | May modify data |
|---|---|---|
| `on_run_started` | Observe the start | No |
| `before_model` | Participate while building a Model Request | A restricted draft |
| `on_model_completed` | Observe an accepted model result | No |
| `on_tool_completed` | Observe an accepted tool result | No |
| `on_run_completed` | Observe Run termination | No |

`before_model` must not receive `&mut AgentState`. It receives explicit input
and a draft:

```rust
pub trait ModelRequestTransform: Send + Sync {
    fn transform(
        &self,
        ctx: &ModelBuildContext,
        draft: ModelRequestDraft,
    ) -> Result<ModelRequestDraft, ExtensionError>;
}
```

It may add system-prompt fragments, adjust the visible tool set, select a model
or tune request parameters. It cannot modify the Run's authoritative state.

If an extension needs external I/O such as a RAG lookup, it must produce a
Context View through a controlled data capability first. A pure-looking hook
must not secretly perform untracked external operations.

### 5.2 Wrap middleware

```text
Model Call
    │
    ▼
Metrics
    │
    ▼
Cache
    │
    ▼
Provider Adapter
```

Model and tool calls can be recorded Effects, which splits wrapping into two
kinds:

- **Behavior Wrap (Harness)** — changes request selection, input construction
  or model routing before the final Effect is accepted.
- **Invocation Wrap (Execution Host)** — wraps the real external call
  (transport metrics, timeouts, controlled retries, caching) and must obey the
  Effect Attempt and Host guarantees.

Why split? A naive retry middleware can write:

```rust
let result = next.call(request.clone()).await;
if result.is_err() {
    return next.call(request).await;
}
```

For an HTTP read that may be fine. If `next` is `git commit` or a payment tool,
it duplicates a side effect. In Autra, any wrapper that can physically retry
must follow the Runtime's `EffectClass`, attempt and recovery policy; it may not
retry privately outside the Host. The same applies to model calls: cache hits,
actual request count and the accepted result must stay consistent with the
execution record.

## 6. Plugin: composition, dependencies and lifecycle

### Plugin contract sketch

```rust
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> PluginManifest;
    fn mount(&self, ctx: &mut PluginRegistrar<'_>) -> Result<(), PluginError>;
}

pub struct PluginManifest {
    pub id: PluginId,
    pub version: Version,
    pub requires: Vec<ServiceRequirement>,
    pub optional: Vec<ServiceRequirement>,
    pub provides: Vec<ServiceDescriptor>,
}
```

`PluginRegistrar` offers typed registration, not an unrestricted mutable global
context.

```rust
impl Plugin for GitPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new("autra.git", "0.1.0")
            .optional_service("ui.panels")
    }

    fn mount(&self, ctx: &mut PluginRegistrar<'_>) -> Result<(), PluginError> {
        ctx.tools().register(GitStatusTool)?;
        ctx.tools().register(GitCommitTool)?;
        ctx.harness().before_model(GitContextExtension)?;
        if let Some(ui) = ctx.ui() {
            ui.register_panel(GitDiffPanel)?;
        }
        Ok(())
    }
}
```

`GitDiffPanel` is a UI contribution descriptor; it does not mean a Rust plugin
injects arbitrary executable code into a browser.

### Service dependencies

A Git plugin that offers a diff UI must still work in a headless agent:

```text
GitPlugin
  ├── Git Tools          required, registered
  ├── Context Extension  registered
  └── Git Diff UI        ui.panels missing → skipped
```

This is what makes mixed capability bundles composable.

## 7. Plugin lifecycle

v1 supports explicit lifecycle but not hot replacement.

| State | Meaning |
|---|---|
| Discovered | Manifest read |
| Resolved | Dependencies, conflicts and capabilities checked |
| Mounted | Tools / hooks / services registered |
| Active | Participates in new Runs |
| Quiescing | No new work; wait for in-flight work |
| Disposed | Registrations and resources cleaned up |

Rules:

- **Dependency checks happen before registration.** Missing required services,
  dependency cycles and duplicate registration IDs fail at startup.
- **Registration is revocable.** If a plugin fails while mounting, already
  registered tools and hooks roll back.
- **Shutdown order is the reverse of dependency order.** Consumers close before
  providers.
- **Unmount is not cancellation of in-flight tools.** An in-flight Run needs a
  drain, an old-version instance, or an explicit refusal to unload.
- **A plugin cannot mutate another plugin's resources.** Every registration has
  an owner.

This borrows Cordis-style resource ownership and revocable registration without
copying a full dynamic-loading system.

## 8. Ordering, scope and durability

### 8.1 Ordering

Registration order is the default:

```text
Request
   ↓
Logging
   ↓
ModelCache
   ↓
ModelRouter
   ↓
Provider
   ↓
ModelRouter
   ↓
ModelCache
   ↓
Logging
   ↓
Response
```

Wrap uses the standard onion model. Lifecycle hooks run in registration order.
Do not add priority numbers, dynamic reordering, event weights or arbitrary
before/after dependencies until a real need proves them.

### 8.2 Scopes

| Scope | Lifetime | Suitable capabilities |
|---|---|---|
| Application | Application start to shutdown | Model providers, global services, UI |
| Agent | Agent Definition lifetime | Tools, context, middleware |
| Run | One execution | Hook context, budgets, temporary state |

Session is an interaction identity and history boundary; it must not become a
shared mutable state container for every plugin. Run-level middleware receives
read-only information, cancellation and restricted capabilities through a
`RunContext`, never another Run's internals.

### 8.3 Durable execution

If a Durable Host is enabled, extensions that affect execution decisions cannot
silently change version on recovery. Record in the Run's execution
configuration:

```text
ExecutionConfiguration
  ├── Agent Revision
  ├── Engine Revision
  ├── Middleware IDs / Versions / Order
  ├── Relevant Config Digests
  └── Schema Versions
```

Recovery checks compatibility. Already-accepted Model Requests or Tool Intents
must not be re-run through middleware to produce a different request that is
then treated as the original Effect. This is a direct consequence of Sans-I/O
and durable execution.

## 9. What extensions may and may not affect

| Extension | Allowed | Forbidden |
|---|---|---|
| Observer | Read committed events, collect metrics | Modify authoritative events |
| Behaviour extension | Modify uncommitted Context / Model Request drafts | Modify a committed Effect |
| Tool | Run operations in an authorized environment | Grant itself extra permission |
| Trusted policy | Deny, require approval, impose constraints | Let an ordinary plugin self-grant higher authority |
| Host adapter | Implement specific execution guarantees | Implicitly claim unverified durable semantics |
| UI extension | Render, submit legal commands | Write RunState or approval records directly |

Execution flow:

```text
Context Builder / Harness
   ↓
Behavior Middleware            build and modify uncommitted requests
   ↓
Final Request / Effect Intent
   ↓
Trusted Execution Boundary     validate · policy · approval · commit
   ↓
Effect Dispatcher / Provider / Tool Host
   ↓
Accepted Outcome / Durable Event
   ↓
Observer Hooks                 record, trace, notify, UI projection
```

A normal hook must not be able to call `ctx.runtime().commit_state(...)`;
otherwise middleware could bypass the Coordinator and the Small Core / Explicit
Effects design loses its meaning.

Two kinds of Effect must stay distinct:

- **Autra Execution Effect** — an Agent's external operation, recorded,
  authorized and recoverable.
- **Plugin Registration Effect** — resources installed and cleaned up by a
  plugin lifecycle.

They may use different terminology and types; they must not share one ledger.

## 10. UI plugins

UI is a **Presentation Capability**, not Agent middleware:

```text
autra-plugin-git
  ├── Git Tools
  ├── Git Context Middleware
  └── Git Diff Renderer
```

Without a Web UI the first two still work. With a UI, the plugin contributes
renderers, panels and configuration pages through a UI registry.

A fuller UI plugin might contain:

```text
autra-ui-web
  ├── Web Server Adapter
  ├── Session Command Client
  ├── Run Event Subscription
  └── UI Extension Registry
```

UI plugins use the public interaction protocol — `SubmitTurn`, `CancelRun`,
`ApproveEffect`, `GetRunSnapshot`, `SubscribeRun` — and never get direct
Runtime memory or host OS access. This keeps CLI, Web, Desktop and third-party
clients equally possible without changing Agent execution semantics.

## 11. Developer experience

Two levels:

**Ordinary Agent — no Plugin knowledge**

```rust
let agent = Agent::builder("assistant")
    .model(model)
    .tool(ReadFile)
    .context(RepoContext)
    .middleware(ModelLogging)
    .build()?;
let result = agent.run("inspect the project").await?;
```

**Full application — Plugin composition**

```rust
let app = Autra::builder()
    .plugin(GitPlugin::new())
    .plugin(BrowserPlugin::new())
    .plugin(WebUiPlugin::new())
    .build()?;

let agent = app.agent("coding-agent")
    .model(model)
    .build()?;
```

The `PluginManager` handles dependencies, resources, hooks and UI registration;
it does not take over the Run's authoritative state. Plugins can arrive through
normal Cargo dependencies; no marketplace or dynamic loader is required.

## 12. Suggested development order

Do not create `autra-plugins`, `autra-middleware` and `autra-ui` immediately.
Build the minimal closed loop on the current architecture:

| Stage | Content | Acceptance |
|---|---|---|
| **E0 Contracts** | Tidy existing Tool / Model / Context / Engine seams | A separate crate can implement and register a capability |
| **E1 Middleware** | Typed lifecycle, request transform, model wrap | Ordering and error propagation defined; committed facts immutable |
| **E2 Plugin composition** | Manifest, dependency checks, registration ownership, lifecycle | One plugin registers several capabilities and cleans them up |
| **E3 Scoped services** | Application / Agent / Run scopes, optional services | No global mutable service locator |
| **E4 UI extensions** | UI registry, panels, tool renderers | Headless and Web reuse the same plugin capabilities |
| **E5 Dynamic plugins** | Hot replacement, runtime loading, isolation | Only when a real need exists |

Suggested module organization (not a required Cargo change):

```text
autra-execution/contracts/   existing capability contracts
autra-harness/middleware/    lifecycle / wrap pipeline
autra/extensions/            plugin composition
```

### Required tests

- Middleware ordering, short-circuiting and error propagation.
- Plugin dependency cycles, missing services, duplicate registration.
- Mount failure rollback, shutdown order, in-flight work cleanup.
- After middleware modifies a request, does the approval digest still match?
- Durable Run recovery with unchanged extension versions.
- Parent/child Run scopes prevent plugin privilege escalation.
- An Agent keeps running after a Web UI plugin unloads.

## 13. Changes to the original design document

Section 7.3 already defines Observer, Behavior Modifier and Policy Authority
trust levels and asks for DSH-style dependencies and lifecycle. This proposal
makes that principle concrete:

1. Keep the §7.3 trust levels.
2. Add a dedicated **Extension Architecture** chapter after §7 defining
   Capability, Middleware, Plugin and scopes.
3. In §9 Tool System, define the boundary between tool wrapping and trusted
   policy.
4. In §11 Session, add presentation / UI extension access rules.
5. In §12 Execution Host, add middleware/durable-effect compatibility.
6. In the roadmap, deliver Middleware first, then plugin lifecycle, then UI
   on demand.

## Final philosophy

**Composable Capabilities · Typed Interception · Scoped Plugins · Protected Execution**

- **Composable Capabilities** — every capability can be implemented and
  composed through a dedicated interface; a plugin is never mandatory.
- **Typed Interception** — lifecycle and wrap points have explicit stages,
  input/output types, ordering and mutation rights.
- **Scoped Plugins** — plugins are manageable composition units with explicit
  dependencies, resource ownership, lifecycle and scope.
- **Protected Execution** — extensions cannot break Core state semantics or
  bypass the Host's authorization, commit and recovery guarantees.

This is more complete than plain middleware and more restrained than a fully
plugin-based microkernel. It takes explicit behaviour interception from
LangChain and plugin/service/lifecycle ideas from DSH/Cordis while preserving
Autra's Small Core, separate execution contracts, composable Harness, trusted
Runtime and optional infrastructure.

## Current status against this proposal

| Proposal element | Repository today |
|---|---|
| Capability registration | Implemented through Builders |
| Typed lifecycle hooks | `ObserverHook` subset only |
| Behaviour wrap | `ModelRequestTransform` via `AgentBuilder::middleware`, before the model Effect is recorded |
| Invocation wrap | Not implemented; a custom `Dispatcher` can wrap manually |
| Plugin composition | Not implemented; design in [`plugins.md`](plugins.md) |
| UI extensions | Not implemented |
| Execution configuration / middleware versions | Not implemented |

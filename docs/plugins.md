# Plugins and Capabilities (Design Only)

**Status: proposal. Nothing in this document is implemented.** Autra currently
extends through Rust traits and Builder composition, as described in
[`extensions.md`](extensions.md). This page records the intended plugin design
so that future work does not accidentally invent a conflicting one.

## Why a plugin layer may be needed

A single product capability often bundles several things. A Git plugin might
contribute:

- Git status and commit tools;
- a command/approval policy for Git operations;
- a diff view in a future UI;
- configuration and credentials discovery;
- initialization and cleanup.

Registering each piece through separate Builder calls works for one plugin and
becomes tedious for ten. The plugin layer should make such a bundle
installable as one unit, without turning every Tool into a plugin.

## Capability vs Plugin

These are different concepts and should stay different:

| Concept | What it is | Examples |
|---|---|---|
| Capability | One executable or declarative contribution | Tool, Model Provider, Context strategy, Policy, UI panel, Service |
| Plugin | A bundle that installs capabilities, validates dependencies and owns a lifecycle | `autra-plugin-git`, `autra-plugin-browser` |

A capability does not have to belong to a plugin. A developer writing one Tool
keeps using:

```rust
Agent::builder("agent").tool(MyTool).build()?;
```

A plugin is for composition, configuration and lifecycle, not a mandatory
wrapper. This follows the project's progressive-complexity principle: simple
things stay simple, and bundles are opt-in.

## Relation to the extension system

The plugin layer is the composition layer of the future extension system
described in [`extension-architecture-v1.md`](extension-architecture-v1.md) and
summarized in [`extensions.md`](extensions.md). It installs four kinds of
contributions:

```text
Autra Extension System
        │
        ├── Capability Registration
        ├── Lifecycle Events
        ├── Wrap Interceptors
        └── Plugin Composition  ← this document
```

A plugin fills capability registries and may contribute lifecycle hooks or
interceptors, but it does not own the execution path. The other three layers
are documented in [`extensions.md`](extensions.md).

## Sketch of the interface

The names below are illustrative; no such types exist in the repository.

```rust
pub trait Plugin: Send + Sync {
    fn metadata(&self) -> PluginMetadata;
    fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError>;
}

pub struct PluginMetadata {
    pub name: String,
    pub version: String,
    pub requires: Vec<ServiceRequirement>,
    pub provides: Vec<ProvidedCapability>,
}

pub struct PluginContext {
    // Capability registries, not a runtime handle.
    tools: ToolRegistry,
    policies: PolicyRegistry,
    services: ServiceRegistry,
    ui: UiRegistry, // future capability
    config: PluginConfig,
}
```

A Git plugin would read roughly like:

```rust
impl Plugin for GitPlugin {
    fn metadata(&self) -> PluginMetadata { /* name, version, requires */ }

    fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.tools().register(GitStatusTool);
        ctx.tools().register(GitCommitTool);
        ctx.policy().register(GitCommandPolicy::default());

        if ctx.services().has("ui.console") {
            ctx.ui().register_panel(GitDiffPanel);
        }
        Ok(())
    }
}
```

Application assembly could then be:

```rust
let app = Autra::builder()
    .plugin(GitPlugin::default())
    .plugin(BrowserPlugin::default())
    .plugin(WebUiPlugin::default())
    .build()?;
```

`ctx.ui()` is a future capability and must not be read as an existing API.

## Where the plugin manager belongs

The plugin manager belongs to the product assembly layer, not to execution:

```text
autra-core
   ↑
autra-execution
   ↑
autra-harness     autra-runtime
        \           /
          autra (PluginManager)
                 ↑
        CLI / Application / UI
```

- Core defines execution semantics and stays indifferent to plugins.
- Execution defines stable contracts (Tool, Model, Journal, Host, ...).
- Harness and Runtime provide behaviour and execution.
- The facade (`autra`, or a future `autra-plugins` crate) assembles plugins
  and application services. It does not drive Runs.
- Tool calls still go through the Execution Runtime; the plugin manager never
  becomes a second execution path.

If the plugin layer grows (UI, policy, service discovery), a separate
`autra-plugins` crate is reasonable. Creating it now would add ceremony
without a second consumer.

## Dependencies and services

Plugins should cooperate through declared services, not by reaching into each
other's internals, in the spirit of Cordis-style service dependencies:

- `metadata().requires` declares services a plugin needs to register at all;
- optional capabilities are checked at registration (`ctx.services().has`);
- a Git UI panel can be skipped when no console/UI service is present, while
  Git tools still work;
- the manager builds a dependency graph, rejects cycles and missing required
  services, and orders initialization and shutdown.

A plugin that requires `ui.console` must not assume that service exists just
because a UI plugin is installed later. The manager either satisfies the
requirement before `register` or fails the installation with a typed error.

## Lifecycle

A future lifecycle has explicit stages:

1. **Discover**: collect plugin instances and metadata.
2. **Validate**: check names, versions, duplicate capabilities and service
   requirements; reject cycles.
3. **Configure**: apply typed configuration before any capability is live.
4. **Register**: add tools, policies, services and UI contributions to
   registries; registration must not start background work.
5. **Start**: start resources after every plugin registered successfully; a
   failed start rolls back earlier starts.
6. **Run**: capabilities are used by Agents through normal execution paths.
7. **Stop / cleanup**: stop in reverse dependency order, release resources,
   close services.

The initial implementation should support deterministic ordering and rollback
on failure. Hot reload, dynamic loading and process isolation are later
questions.

## Security and authority

A plugin is not a trust boundary. It contributes capabilities; the trusted host
still decides what may execute:

- a plugin-provided policy may narrow permissions, never widen them beyond the
  host's grants;
- tool approval, sandboxing, budgets and durable commit stay in the execution
  layer;
- plugin code is untrusted by default unless the application explicitly trusts
  it;
- a plugin must not receive a raw host handle or the ability to bypass the
  Dispatcher/Approver;
- UI contributions are presentation, not authorization.

If plugins later load native code or remote packages, isolation, signing and
capability grants become separate infrastructure work. None of that is claimed
by this design.

## Non-goals for the first plugin slice

- no dynamic library loading, hot reload or hot swap;
- no web UI or panel protocol;
- no MCP server/client;
- no process or sandbox isolation for plugin code;
- no plugin marketplace, version solver or remote installation;
- no second execution path outside the Runtime.

## Suggested phases

| Phase | Content | Exit criterion |
|---|---|---|
| P0 | `PluginMetadata`, `Plugin`, `PluginContext` with a Tool registry; `AutraBuilder::plugin` | A Git plugin registers its tools in one call; direct `.tool()` still works |
| P1 | Service registry, required/optional services, dependency graph and cycle errors | UI contribution is skipped when `ui.console` is absent; tools still work |
| P2 | Policy contributions with host-side narrowing checks | A plugin cannot grant more than the host allows; contract test proves it |
| P3 | Lifecycle start/stop and rollback | A failing plugin start leaves no live capabilities |
| P4 | UI/service protocol and optional dynamic loading | Only after the core protocols above are stable |

## Acceptance criteria for P0

- A plugin can register multiple tools; failing registration rolls back the
  whole plugin.
- Plugin metadata is reported through the manager without executing the plugin.
- Duplicate tool names or conflicting metadata produce a typed error.
- `AgentBuilder::tool` remains a supported path with no plugin involved.
- The plugin manager lives in the facade; neither Core, Execution, Harness nor
  Runtime imports it.

## Open questions

- configuration schema and validation (typed Rust structs vs. JSON);
- version compatibility policy for plugins and services;
- whether a plugin may register another plugin;
- UI protocol: in-process panels vs. a remote client protocol;
- how plugin-provided policies interact with a future policy registry;
- isolation model if plugins become third-party native code.

Until these are answered, the repository keeps the Builder-based extension
model documented in [`extensions.md`](extensions.md).

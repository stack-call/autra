# Host and Sandbox Declare Capabilities Before Adapters

**Status:** accepted

Different execution hosts and sandboxes guarantee different things. Treating a
SQLite host, Restate host or OpenShell backend as interchangeable would silently
upgrade or downgrade recovery and isolation.

We added `HostCapabilities` (`durable_wait`, `crash_recovery`,
`effect_reconciliation`, `workspace_snapshot`) and a `SandboxBackend` contract
with `SandboxCapabilities` and provision/execute/inspect/release lifecycle.
`LocalHost` declares no durable guarantee. No external host or sandbox adapter
is wired in yet.

Consequences: adapters can declare rather than infer; unsupported operations
return typed errors. A future runtime integration must bind the sandbox lifecycle
to a Run/Workspace and run conformance tests per host.

Alternatives considered: boolean-free capability strings (not yet needed), a
single storage plugin interface (confuses journal ownership), and implementing
OpenShell immediately (premature). Revisit with the first real adapter.

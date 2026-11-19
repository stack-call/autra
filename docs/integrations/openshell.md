# OpenShell Integration

**Status:** not implemented. This page records the intended boundary.

OpenShell provides a gateway, sandbox lifecycle, a trusted supervisor and
enforcement for filesystem, process and network policy. Autra would use it as a
**sandbox backend**, not as a durable workflow engine.

## Two placements

- **Tool sandbox**: the Agent Core runs in the trusted host; only Tools run in
  OpenShell. This is the first target.
- **Whole-Agent sandbox**: the Agent worker and its Tools run inside the
  controlled environment. This is for stronger tenant isolation.

## Capability matrix

A `SandboxBackend` adapter must declare, rather than assume:

- persistent workspace;
- workspace clone;
- snapshot and restore;
- command execution;
- file API;
- network policy;
- credential injection;
- CPU/memory limits;
- retention period.

An unsupported capability returns a typed error. `release()` is not a promise
that the workspace can be recovered later.

## Environment recovery

| Situation | Can execution continue? |
|---|---|
| Read-only tools, no environment dependency | Possibly, after checking data versions |
| Persistent workspace with matching version | Possibly, after checking steps, versions and permissions |
| Lost workspace or version conflict | No; rebuild, reconcile or ask a human |

## What OpenShell does not do

It does not recover Agent execution logic, and it does not replace a durable
journal. See [`ADR-0009`](../adr/0009-openshell-is-a-sandbox-backend.md) and
[`../durability.md`](../durability.md).

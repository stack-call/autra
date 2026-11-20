# Tool Security

## Two halves of a Tool

A **Tool Description** is for the model: name, description, input schema and
constraints. An **Execution Contract** is for the runtime: who may call it,
where it runs, its resource limits, whether it can write external state and how
an unknown result is handled.

The current MVP implements the description plus a minimal contract
(`effect_class`, arguments, result). Approval is implemented for
`write_file`; credential injection and a per-tool policy registry are not.

## Effect classes

| Class | Examples | Retry posture |
|---|---|---|
| ReadOnly | read_file, list_files | Usually safe |
| Idempotent | update with an idempotency key | Same key, verify the service |
| Reconcileable | git commit, conditional write | Query the real state first |
| NonIdempotent | payment, email | Never blind-retry |
| Unknown | unclassified third-party tool | Conservative; require approval |

`EffectClass` is a hint for execution and review, not an external guarantee.

## Current enforcement

- A model tool call is a **request**, not an authorization.
- The runtime resolves the tool by name; unknown tools become error outcomes.
- Arguments are raw JSON strings; each tool validates its own input.
- `LocalWorkspace` canonicalises the root and rejects absolute paths,
  parent-directory traversal and symlinks resolving outside the root.
- `read_file` caps output, truncates on a UTF-8 boundary, and never buffers the
  whole file.
- `list_files` caps entries and marks directories.
- Built-in read-only tools are `ReadOnly`. `search_text` walks the workspace,
  skips symlinks, `.git`, `target` and `node_modules`, caps file size and result
  count, and never leaves the root.
- `write_file` declares `Idempotent`, requires approval, creates missing parent
  directories inside the workspace and caps content at 1 MiB.
- `run_command` declares `NonIdempotent`, requires approval, runs an
  unsandboxed shell in the workspace root, caps output at 32 KiB and defaults
  to a 60-second timeout. A timed-out command is reported as `OutcomeUnknown`
  because it may already have changed the workspace. `--read-only` removes it
  and `write_file`; `--yes` skips its approval prompt.
- Before dispatching an approval-required call, the runtime asks an `Approver`.
  The request binds the Run id, call id, tool name, arguments and a SHA-256
  argument digest. A rejected call or a missing approver produces an error
  outcome and does not execute.

`LocalWorkspace` is **user-space path validation, not a sandbox**, and
`run_command` is an unsandboxed shell convenience. Neither isolates processes,
mounts, environment variables or the network.

## Threat boundaries

| Threat | Required boundary |
|---|---|
| Prompt injection | Independent tool policy/approval; least privilege |
| Untrusted shell | Sandbox, resource limits, host file/network policy |
| Credential leakage | Credential broker, controlled injection, log redaction |
| Path traversal | Trusted file API plus directory/mount isolation |
| Child Agent escalation | Child scope can only shrink |
| Budget exhaustion | Turn/tool/child limits, depth limits, backpressure |
| Approval reuse | Bind approval to parameters, actor, policy revision and expiry |

## What is not claimed

- A Rust trait, capability field or hook is not an OS security boundary.
- A local shell mode is not equivalent to OpenShell isolation.
- Approval is local and ephemeral: there is no expiry, actor identity or
  policy registry yet. Shared workspace writes still need a version-check or
  worktree policy before parallel writes are safe.

See [`ADR-0007`](adr/0007-tool-effect-and-authority-separated.md),
[`ADR-0009`](adr/0009-openshell-is-a-sandbox-backend.md) and
[`integrations/openshell.md`](integrations/openshell.md).

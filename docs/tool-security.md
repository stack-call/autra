# Tool Security

## Two halves of a Tool

A **Tool Description** is for the model: name, description, input schema and
constraints. An **Execution Contract** is for the runtime: who may call it,
where it runs, its resource limits, whether it can write external state and how
an unknown result is handled.

The current MVP implements the description plus a minimal contract
(`effect_class`, arguments, result). Approval is implemented for `write_file` and `run_command`; a local policy registry now handles allow/ask/deny rules. Credential injection and verified actor identity are not implemented.

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
- `edit_file` declares `Idempotent`, requires approval, and replaces an exact
  string only when it is unique; an ambiguous match is rejected unless
  `replace_all` is set. The result includes a bounded unified diff.
- `write_file` declares `Idempotent`, requires approval, creates missing parent
  directories inside the workspace and caps content at 1 MiB.
- `git_status` and `git_diff` are `ReadOnly`, run `git` in the workspace root
  and are bounded to 32 KiB. `git_diff` limits a requested path to the
  workspace.
- A tool may return a `preview` of its pending change. The runtime attaches it
  to `ApprovalRequest`, and the CLI prints it before asking for approval. The
  preview is guidance for a human; it does not authorize anything and cannot
  widen what the tool may do.
- `run_command` declares `NonIdempotent`, requires approval, runs an
  unsandboxed shell in the workspace root, caps output at 32 KiB and defaults
  to a 60-second timeout. A timed-out command is reported as `OutcomeUnknown`
  because it may already have changed the workspace. `--read-only` removes it
  and `write_file`; `--yes` skips its approval prompt.
- Before dispatching a gated call, the runtime evaluates `PolicyChain`:
  a host baseline (`Ask` for `Unknown` effect class or `requires_approval`,
  otherwise `Allow`) combined with ordered `ToolPolicy` rules by severity.
  `Deny` wins over `Ask`, and `Ask` wins over `Allow`, so a plugin rule cannot
  widen the baseline.
- `RulePolicy` supports allow/ask/deny tool lists and substring deny patterns
  for `run_command`. The CLI loads it from `--policy <path>`; deny rules also
  apply under `--yes`. Deny patterns are a coarse guard, not a sandbox.
- Decisions are persisted in an `ApprovalStore` and reused on resume only when
  the effect, argument digest and policy revision match and the record has not
  expired. `autractl approve`/`reject` support headless approval.
- The runtime asks an `Approver` for `Ask` calls. The request binds the Run id,
  call id, tool name, arguments, policy revision, actor and an expiry timestamp
  (default five minutes), plus a SHA-256 argument digest. A `Deny`, a rejected
  decision or a missing approver produces an error outcome and does not execute.
- Unknown tools are dispatched as normal tool errors rather than gated.

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

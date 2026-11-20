# Tool approval and guarded writes

**Status:** ready-for-agent

**Source:** design spec §9.2 (tool lifecycle), §11.3 (approval binding),
§18.2 (approval reuse), and the M3 milestone.

## Problem Statement

The harness can only run read-only tools. Any write would execute on the
model's request alone, with no human decision and no binding between the
approval and the concrete call.

## Solution

Add a Tool approval contract. A Tool declares whether it requires approval;
before dispatching such a call, the runtime asks an `Approver` and binds the
decision to the Run, call id, tool name and an argument digest. The first write
tool, `write_file`, requires approval and stays confined to the Workspace.

## Implementation Decisions

- `Tool::requires_approval()` defaults to false. `WriteFileTool` returns true.
- `ApprovalRequest` carries `run_id`, `call_id`, `tool_name`, `arguments`,
  `argument_digest` (SHA-256 over tool name and arguments) and a policy
  revision string. There is no expiry or identity store in this local slice.
- `Approver` is an async trait returning `Approved` or `Rejected`.
- An Agent may set a default approver; `RunOptions` may override it per Run.
  With no approver, an approval-required call is rejected.
- Approvals are resolved sequentially for a batch before any call executes.
  Rejected calls become `is_error` ToolOutcomes and do not touch the tool.
  Approved calls may still execute with the configured parallelism.
- Stream events report `ApprovalRequested` and `ApprovalDecided`.
- `write_file` accepts a workspace-relative path and UTF-8 content, refuses
  absolute/traversal/symlink-escape paths, caps content at 1 MiB, and returns
  a short write summary. It declares an idempotent effect class because
  overwriting the same content is repeatable; approval still guards the call.
- No durable WaitingApproval state, expiry, actor identity or policy registry
  yet. Those belong to the durable host slice.

## Testing Decisions

- Runtime seam: approved write executes; rejected and no-approver calls do not
  write and return error outcomes; approval events carry the digest; a batch
  with one approval-required call and one read-only call works.
- Tool seam: write_file path confinement, content cap, overwrite behaviour.
- Approval digest changes when arguments change.

## Out of Scope

Durable approval waits, expiry, actor identity, policy registries, shell
execution, apply_patch, and OS sandboxing.

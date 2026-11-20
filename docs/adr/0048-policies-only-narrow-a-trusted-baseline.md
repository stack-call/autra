# Policies Only Narrow a Trusted Baseline

**Status:** accepted

Approval was driven by `ToolContract::requires_approval` alone. A tool with an
`Unknown` effect class that forgot to set the flag executed without a check,
and there was no way to deny a tool or a dangerous command per workspace. The
CLI had only "ask every time" or `--yes`.

Tool admission is now a policy decision:

- `PolicyRequest` carries run id, tool name, arguments, effect class and the
  tool's own approval flag;
- `PolicyDecision` is `Allow`, `Ask` or `Deny`;
- `ToolPolicy` contributes one ordered rule;
- `PolicyChain` starts from the host baseline (`Ask` for `Unknown` or
  `requires_approval`, otherwise `Allow`) and combines rules by severity:
  `Deny` > `Ask` > `Allow`.

`RulePolicy` in the runtime supports allow/ask/deny tool lists and substring
deny patterns for `run_command`. The CLI loads it from `--policy <path>` as
JSON, and `--actor <name>` records the operator on approval requests.
`ApprovalRequest` now carries policy revision, actor and an expiry timestamp
(default TTL five minutes).

The engine stages effects as `AwaitingApproval` when the decision is `Ask` or
`Deny`, and the tool batch denies without consulting an approver. Unknown tools
are never gated; they are dispatched so the model receives a normal unknown-tool
error. Approval-required tools keep the old prompt path.

Consequences: an `Unknown` tool cannot execute silently, a workspace can deny a
tool or command even under `--yes`, and a plugin-provided rule returning `Allow`
cannot lower the host baseline. Deny patterns are a coarse guard, not a
sandbox. Approval expiry is metadata for the approver; approvals are still not
persisted, and actor identity is claimed rather than verified.

Alternatives considered: adding policy checks inside each Tool (untrusted and
duplicated), a single global allow/deny list (no baseline), and making every
policy decision `Deny` by default (unusable for read-only tools).

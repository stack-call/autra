# Workspace Autonomy Is Approval-Bound

**Status:** accepted

The CLI could chat with a model, and `demo` exercised the read-only workspace
tools, but `run` and `chat` registered no tools. An agent could not inspect a
real workspace, edit it or verify a change, so it could not work independently.

`autractl run` and `autractl chat` now build a workspace agent:

- `--workspace <dir>` (`-C`) selects the root; every file tool is confined to
  it through `LocalWorkspace`;
- `list_files`, `read_file` and the new `search_text` are always available and
  are `ReadOnly`;
- `write_file` (which now creates missing parent directories) and the new
  `run_command` are registered unless `--read-only` is passed, and both require
  approval;
- `--yes` auto-approves, otherwise the CLI asks on stdin before each write or
  command;
- the agent gets a workspace-oriented system prompt and larger turn/tool
  budgets.

`run_command` runs `cmd /C` on Windows and `sh -c` elsewhere, with the
workspace as the working directory, a 60-second default timeout, a 32 KiB
output cap and `NonIdempotent` classification. A timeout is reported as
`OutcomeUnknown` through the new `ToolError::outcome_unknown` flag, because the
command may already have written to the workspace.

Consequences: a run can complete a small workspace task end to end. This is
user-space path validation plus approval, not isolation: commands inherit the
environment and have full local process rights. `--read-only` and `--yes` are
the operator's only coarse controls; there is no policy registry, actor
identity, approval expiry or OS sandbox yet. `search_text` skips symlinks,
`.git`, `target` and `node_modules` to stay useful and bounded, which is a
heuristic rather than a security boundary.

Alternatives considered: keeping the CLI chat-only (not independently useful),
shipping `run_command` without approval (unacceptable for a default), and
requiring OpenShell before any local command (blocks the harness on unbuilt
infrastructure).

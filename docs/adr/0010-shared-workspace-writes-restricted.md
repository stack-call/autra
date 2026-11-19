# Shared Workspace Writes Are Restricted

**Status:** accepted

Two parallel tools or Subagents editing one workspace can silently overwrite
each other. Parallel execution must not imply that every write is safe.

We decided shared workspace writes are serial or version-checked by default.
Independent Agents that need to edit concurrently should use separate
worktrees/workspaces and merge explicitly. Read-only parallel calls are the
safe default.

Consequences: some parallelism is lost. The MVP only ships read-only tools, so
the rule is documented before write tools arrive.

Revisit when a conditional-write or workspace-version protocol is implemented.

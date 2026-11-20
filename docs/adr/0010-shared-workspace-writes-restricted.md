# Shared Workspace Writes Are Restricted

**Status:** accepted

Two parallel tools or Subagents editing one workspace can silently overwrite
each other. Parallel execution must not imply that every write is safe.

We decided shared workspace writes are serial or version-checked by default.
Independent Agents that need to edit concurrently should use separate
worktrees/workspaces and merge explicitly. Read-only parallel calls are the
safe default.

Consequences: some parallelism is lost. `write_file` now exists behind
approval, but approval does not serialize concurrent writes; the default
parallel limit stays 1 and shared writes still need a version/worktree policy.

Revisit when a conditional-write or workspace-version protocol is implemented.

# Approval Shows a Tool-Provided Preview

**Status:** accepted

`write_file` could only replace a whole file, and the approver saw a tool name,
a call id and a digest. A digest proves nothing to a human: approving a write
or a shell command required trusting the model's description or reading the
arguments by hand.

Three changes make the approval surface usable:

- `edit_file` replaces an exact string, reports a bounded unified diff, and
  refuses a missing, identical or ambiguous match unless `replace_all` is set.
- The `Tool` contract gained `preview(&self, arguments) -> Option<String>`.
  `write_file` previews a create or an overwrite diff, `edit_file` previews the
  resulting diff or the reason the edit would be rejected, and `run_command`
  previews the command.
- `ApprovalRequest` carries `preview`. The runtime computes it from the
  registered tool before asking the approver; the CLI prints it, then asks
  `y/N`. Without a preview the raw arguments are shown.

Read-only `git_status` and `git_diff` tools let the agent check the workspace
before and after a change, so the preview and the recorded result can be
compared with real repository state.

Consequences: an approving human sees what will change, not a hash. The
preview is advisory: it reads the file at approval time, does not reserve the
path, and could differ from what the tool later writes if something else
modifies the file. It is not a transaction, a lock or authorization. Approval
still binds to the argument digest, so a changed argument requires a new
approval. Rollback is not implemented; the tools return diffs but do not store
pre-images.

Alternatives considered: rendering the diff only in the CLI from raw arguments
(the CLI lacks the workspace and the tool's validation rules), adding a diff
library to every host instead of the tool contract (duplicated policy), and
leaving approval as a digest prompt (not usable for real edits).

# 01: Tool approval and write_file

**What to build:** A developer can register a write tool that only executes after an Approver approves the concrete call. A rejected or unconfigured approval leaves the workspace untouched, and the model receives a clear error outcome.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `Tool::requires_approval()` defaults to false; `write_file` requires approval.
- [x] `ApprovalRequest` binds run id, call id, tool name, arguments and a SHA-256 digest.
- [x] An Agent default Approver is overridable per Run; no approver means rejection.
- [x] Approved calls execute; rejected calls produce error outcomes and no write.
- [x] `ApprovalRequested` and `ApprovalDecided` run events are emitted.
- [x] A mixed batch resolves approvals before executing read-only calls.
- [x] `write_file` stays inside the workspace, caps content and reports its result.
- [x] Tests cover approved, rejected, missing approver, digest binding and path escapes.

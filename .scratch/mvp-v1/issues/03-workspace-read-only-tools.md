# 03: Workspace read-only tools

**What to build:** A caller can give an Agent a LocalWorkspace and the built-in read_file and list_files tools. The model can inspect a directory and read a file inside that workspace, while absolute paths, parent-directory traversal and symlink escapes are rejected. Output is bounded and explicitly marked when truncated. An Agent can complete a list-then-read task against a temporary workspace.

**Blocked by:** 02: Tool-using ReAct loop

**Status:** ready-for-agent

- [ ] read_file returns UTF-8 content for a workspace-relative file.
- [ ] Absolute paths, parent-directory components and symlinks resolving outside the workspace are rejected.
- [ ] read_file caps output at 32 KiB by default and marks truncation; the cap is overridable.
- [ ] list_files returns sorted entries, suffixes directories with a slash, caps at 200 entries and marks truncation.
- [ ] An Agent end-to-end test lists and reads a file in a temporary workspace.
- [ ] Both built-in tools declare the ReadOnly effect class.
- [ ] Format, clippy with warnings denied, and workspace tests pass.

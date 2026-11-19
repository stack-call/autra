# 05: autractl demo and CLI surface

**What to build:** A user can run a deterministic demo from the terminal that lists the workspace, reads README.md and prints a final answer without credentials. The CLI also lists built-in tools, prints help, truncates long tool output for display, and rejects unknown commands with usage. The README explains how to build, run and test the project.

**Blocked by:** 03: Workspace read-only tools

**Status:** done

- [x] `autractl demo` completes the list-files, read-README, final-answer sequence and exits successfully.
- [x] `autractl tools` lists read_file, list_files and echo with descriptions.
- [x] `autractl --help` lists the run, demo and tools commands.
- [x] CLI display truncates tool content to 400 characters while event payloads keep the full content.
- [x] An unknown command exits non-zero and prints usage.
- [x] README documents build, run, demo and test commands.
- [x] CLI smoke tests and workspace tests pass.

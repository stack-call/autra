# 01: Restricted subagents

**What to build:** A parent Agent can delegate a task to a named child Agent with its own model, tools and budgets, and receive the child's answer as a tool result.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `SubagentRegistry` holds named child Agents and descriptions.
- [x] `AgentBuilder::subagent` registers a child and exposes a `spawn_subagent` tool.
- [x] The tool schema enumerates registered names; unknown names become error outcomes.
- [x] The child runs as a fresh Agent Run with its own definition and budgets.
- [x] Child failures become error outcomes so the parent model can react.
- [x] Tests cover a successful child, unknown child and child failure.
- [x] Docs and an ADR record the tool-backed (not runtime ChildRun) boundary.

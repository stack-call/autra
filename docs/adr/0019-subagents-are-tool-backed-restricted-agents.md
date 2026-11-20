# Subagents Are Tool-Backed Restricted Agents

**Status:** accepted

A full runtime ChildRun needs parent scope, cancellation propagation, a budget
slice, a Join policy and an execution identity. Building all of that before a
real delegation need exists would add complexity to the coordinator.

We decided a first Subagent is a named child `Agent` behind the
`spawn_subagent` tool. It has its own model, tools, approval configuration,
context and budgets. The parent receives the child answer as a ToolOutcome; a
child failure is an error outcome.

Consequences: the child's permissions are exactly its own tool set, and the
parent does not inherit its state. There is no cancellation propagation, shared
budget slice or Join policy; a child can run to its own configured limits.
Recursion is possible only if the user explicitly gives a child the same
subagent capability.

Alternatives considered: a full `SpawnChild` Effect now (larger coordinator
change), in-process function calls (no isolation or budget), and a separate
service (premature). Revisit when delegation needs cancellation or shared
budget accounting.

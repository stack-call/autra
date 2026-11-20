# Child Budgets Are Independent and Capped

**Status:** accepted

ADR-0049 made `spawn_subagent` reserve four turns and four tool calls from the
parent budget. The reservation was never refunded, so a parent with the default
eight tool calls could spawn at most one child per Run; a second spawn failed
even when the first child used no tools.

A child Run has its own budget. `spawn_subagent` now gives the child
`BudgetSnapshot::new(4, 4.min(parent_remaining_tool_calls))` without charging
the parent. The spawn itself remains one parent tool call. A parent with no
remaining tool calls can still spawn a child, but that child gets a zero
tool-call allowance and cannot call tools.

`BudgetHandle::reserve_child` remains available for future explicit budget
slicing; it is no longer used by the default subagent tool.

Consequences: parents can spawn multiple children in a Run, and child tool use
cannot silently exhaust the parent's tool budget. Parent `max_tool_calls` no
longer bounds how many tools a child may use beyond the initial cap; the child's
own budget is the authority. Child usage is not settled against the parent,
which keeps the two scopes independent.

Alternatives considered: refunding unused reservations after each child (fixes
sequential spawns but not concurrent spawns in one batch), and charging actual
child usage to the parent (conflates two Run budgets and needs a new
usage-reporting contract).

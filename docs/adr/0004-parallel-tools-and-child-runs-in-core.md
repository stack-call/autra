# Parallel Tools and Child Runs Are Core Semantics

**Status:** accepted; partial implementation

Fan-out, Join, budgets and cancellation are not an optional orchestration
plugin: if they are bolted on, orphan tasks and unbounded child recursion
become possible.

We decided structured concurrency is part of Core semantics. A parent Run owns
child budget and cancellation; parallel tool results are merged under
controlled state commit rules.

Consequences: cancellation, Join policy, budget slicing and write-conflict
policy are mandatory design concerns. Parallel tool execution and runtime
ChildRun identity/cancellation were implemented later (ADR-0045, ADR-0054);
parent budget settlement and write-conflict policy remain open.

Revisit only if the complexity cannot be justified by real Agent workloads.

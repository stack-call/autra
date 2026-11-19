# ReAct Is the Only Engine Initially

**Status:** accepted

Designing a universal Planner or Graph abstraction before a second real engine
exists produces speculative generality and a harder API.

We decided to implement ReAct first. `AgentStepper` remains a public seam, but
the builder wires only `ReActStepper`. A second engine is the trigger for
extracting shared abstractions.

Consequences: no custom Stepper injection in the MVP; some users may want one
earlier. The lack is explicit at the API rather than hidden behind a hook.

Revisit when a concrete non-ReAct engine is required by a real workload.

# Evaluation Is Outcome-Based

**Status:** accepted

It is tempting to call a passing task suite proof that the framework is
reliable. Task success and runtime correctness are different claims.

We added `autra-eval` to measure Agent outcomes: a task passes when the answer
contains the expected text, and errors are recorded. Runtime fault tests remain
the evidence for duplicate effects, cancellation races, orphan tasks and
recovery.

Consequences: one suite can show a regression in task behaviour; it cannot
show that no effect was duplicated. Reports must name the model, tools and
budget used.

Alternatives considered: treating traces as evaluation (lossy), a single demo
(too narrow), and an external benchmark first (no local signal). Revisit when
there is a durable host and reproducible fault evaluation.

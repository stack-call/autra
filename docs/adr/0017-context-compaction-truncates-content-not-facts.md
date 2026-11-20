# Context Compaction Truncates Content, Not Facts

**Status:** accepted

A context budget must reduce prompt size without deleting the evidence needed
for replay, tool-call pairing or audit.

We decided `SimpleContextBuilder` truncates the oldest oversized Tool and
Assistant content to 512 characters with a `[context budget]` marker. It never
removes an item, a tool-call id or a tool outcome. The budget is a character
proxy, not a tokenizer. Skills are listed by name and loaded through the
read-only `load_skill` tool; loading does not grant permissions.

Consequences: prompts lose detail but execution facts survive. A 512-character
cap may be too coarse for some models; a tokenizer-aware strategy can replace
the builder without changing the Stepper. Summarization and retrieval memory
are not implemented.

Alternatives considered: dropping the oldest turns (breaks tool-call pairing),
summarizing (extra model cost and loss), and a tokenizer dependency (deferred).
Revisit when a tokenizer and evaluation fixtures justify a more precise budget.

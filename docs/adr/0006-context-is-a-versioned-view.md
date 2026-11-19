# Context Is a Versioned View

**Status:** accepted

It is tempting to treat the prompt or message history as the source of truth.
Then compaction, truncation or a context rewrite can silently delete an
approval or a completed tool call.

We decided the Context View is a versioned model input, separate from the
authoritative journal. Compaction may change what the model sees but never
deletes or rewrites accepted execution facts.

Consequences: Context Records need provenance and builder revisions; summaries
are model input, not facts. The MVP uses history directly as its context and
does not compact yet.

Revisit when compaction or memory retrieval is introduced; the separation must
remain.

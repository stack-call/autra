# OpenAI-Compatible Provider Is a Separate Crate

**Status:** accepted

The framework needs a real model provider without putting HTTP, API keys or
provider JSON into Core or making the harness depend on a network stack.

We added `autra-openai`, which implements the existing `ModelProvider` seam for
OpenAI-compatible chat completions. It is non-streaming, configured through
`OPENAI_API_KEY` and optional `OPENAI_BASE_URL`, and never retries silently.

Consequences: the default framework build stays network-free; the CLI enables
the provider only when `--model` is passed. Streaming, provider-specific fields
and automatic retries remain out of scope. A fixed provider-side HTTP timeout
was removed so the runtime `effect_timeout` remains the single timeout
authority; the CLI sets a 120-second Effect timeout for real runs.

Alternatives considered: a feature flag inside `autra-harness` (smaller crate
count, heavier harness) and multiple provider SDKs (premature). Revisit when
streaming or a second protocol shape is required.

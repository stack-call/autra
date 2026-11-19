# OpenAI-compatible model provider

**Status:** ready-for-agent

**Source:** MVP v1 spec, Out of Scope: the real model provider is the next slice.

## Problem Statement

MVP v1 ships only `EchoModel` and `FakeModel`, so a developer cannot run an
Autra Agent against a real model. The `ModelProvider` seam exists, but no
provider implementation uses it.

## Solution

Add an optional `autra-openai` crate that implements `ModelProvider` against
the OpenAI chat completions API, and let the CLI select it with
`autractl run --model <name>`. Configuration comes from environment variables,
so no credential is baked into the binary or the repository.

## Implementation Decisions

- New crate `autra-openai`, depending on `autra-core` and `autra-harness`.
  The harness stays free of HTTP dependencies.
- `OpenAiConfig { base_url, api_key, model }` can be constructed explicitly or
  from the environment. `OPENAI_API_KEY` is required; `OPENAI_BASE_URL`
  defaults to `https://api.openai.com/v1`.
- Only non-streaming `complete()` is implemented, matching the existing
  `ModelProvider` trait. Streaming remains out of scope.
- Request mapping: system instructions become a system message; user,
  assistant and tool history map to the corresponding chat messages; tool
  specs map to OpenAI function tools with `tool_choice = "auto"`.
- Response mapping: assistant text becomes `ModelOutput.text`; each tool call
  becomes `ToolCallRequest` using the provider's call id. An empty response
  yields an empty `ModelOutput` and is left for the Stepper to interpret. A
  response without `choices[0].message` is a malformed-response error.
- Non-2xx responses and malformed JSON return `ModelError` with a truncated
  body. The provider imposes no HTTP timeout of its own; the runtime `effect_timeout` governs it. The CLI sets a 120-second Effect timeout for real-provider runs.
- The CLI keeps `EchoModel` as the default. `--model <name>` selects the
  OpenAI-compatible provider and fails clearly when credentials are missing.
- No retries, provider-specific extensions, token accounting or embeddings.

## Testing Decisions

- Unit-test request building and response parsing as pure functions.
- Use a wiremock server to verify the POST path, bearer token and parsed
  response without contacting a real provider.
- CLI test: `run --model` without `OPENAI_API_KEY` exits non-zero with a clear
  message; an empty model name is a usage error.
- Cover null tool arguments and malformed response bodies.
- No live provider call in the default test suite. A real endpoint can be
  exercised manually with credentials.

## Out of Scope

Streaming responses, retries, provider-specific fields, embeddings, token
counts, multi-provider abstraction beyond the OpenAI-compatible shape, and any
change to approval, durability or concurrency semantics.

## Further Notes

This slice must not change `autra-core`. The provider is an adapter behind the
existing `ModelProvider` seam.

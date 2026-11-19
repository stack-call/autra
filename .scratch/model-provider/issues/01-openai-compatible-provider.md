# 01: OpenAI-compatible model provider

**What to build:** A developer can run an Autra Agent against an OpenAI-compatible chat completions endpoint by selecting a model name in the CLI. The provider maps Autra's system instructions, message history, tool specs and tool results to the chat API, and maps text and tool calls back. Credentials come from the environment, and missing credentials fail with a clear message instead of a stack trace.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `autra-openai` implements `ModelProvider` for non-streaming chat completions.
- [x] `OpenAiConfig` supports explicit construction and `from_env`, reading `OPENAI_API_KEY` and optional `OPENAI_BASE_URL`.
- [x] Request mapping covers system, user, assistant tool-call history, tool results and function tool specs.
- [x] Response parsing covers assistant text, tool calls, object-shaped arguments,
  null arguments and empty assistant messages; malformed responses are errors.
- [x] Non-2xx responses and malformed JSON return a clear `ModelError` with a truncated body.
- [x] A wiremock test verifies the POST path, bearer token and response parsing.
- [x] `autractl run --model <name> <prompt>` selects the provider; a missing key exits non-zero with a clear message.
- [x] README documents the environment variables and an example invocation.
- [x] Format, clippy with warnings denied, and workspace tests pass.

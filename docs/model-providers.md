# Model Providers

## The seam

`autra-harness::ModelProvider` is the only boundary between the harness and a
model:

```text
complete(ModelCallSpec) -> Result<ModelOutput, ModelError>

ModelCallSpec { system, messages, tools }
ModelOutput   { text, tool_calls }
```

It is async, object-safe and provider-agnostic. Core never sees HTTP, API keys
or provider-specific JSON.

## Implementations

| Provider | Purpose | Network |
|---|---|---|
| `EchoModel` | CLI smoke tests; returns the last user input prefixed with `echo:` | No |
| `FakeModel` | Deterministic scripted ReAct and failure tests | No |
| `OpenAiCompatibleModel` | Real chat completions (`autra-openai` crate) | Yes |

## OpenAI-compatible configuration

```text
OPENAI_API_KEY   required
OPENAI_BASE_URL  optional; default https://api.openai.com/v1
OPENAI_MODEL     optional; default model when --model is omitted
```

```bash
OPENAI_API_KEY=... cargo run -p autra-cli -- run --model gpt-4o-mini "Explain this crate"
```

The CLI also reads these values from a `.env` file so they do not have to be
exported in every shell:

```text
./.env            checked first
~/.autra/.env     fallback
AUTRA_ENV_FILE    explicit path; must be readable
```

The file uses plain `KEY=VALUE` lines (comments and quoted values are
accepted). Process environment values win over the file. When the file sets
`OPENAI_MODEL`, `--model` becomes optional; `--local` still forces the echo
model. The provider crate itself never reads files; the CLI resolves both
sources and passes an explicit `OpenAiConfig` (ADR-0013).

The same adapter targets DeepSeek, Moonshot, vLLM, Ollama, LM Studio and other
OpenAI-compatible endpoints through `OPENAI_BASE_URL`.

## Request and response mapping

- system instructions → a `system` message;
- user, assistant (text plus tool calls) and tool outcomes → chat messages;
- ToolSpec → an OpenAI function tool with `tool_choice = "auto"`;
- response text → `ModelOutput.text`;
- response tool calls → `ToolCallRequest` with the provider's call id;
- object-shaped `arguments` are serialized to a JSON string;
- non-2xx and malformed JSON → `ModelError` with a truncated body.

An empty assistant message is allowed and completes the Run with no answer.

## Capability declaration

`ModelCapabilities` declares `tool_calling`, `streaming`, `structured_output`,
`multimodal_input` and `max_context_tokens`. The OpenAI-compatible adapter declares
tool calling; Echo and Fake use defaults (Fake declares tool calling for scripted tests).
Harness consumers must not assume a capability; unsupported features should return
typed errors. Provider-specific fields stay inside the adapter.

## What is not implemented

- token streaming (`complete()` returns the whole response);
- retries or provider-side rate-limit handling;
- provider-specific fields, reasoning traces or embeddings;
- live-provider tests in the default suite.

The runtime's `effect_timeout` applies to the whole model call. If a provider
call times out, the Run fails; there is no hidden retry.

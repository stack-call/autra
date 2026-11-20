# Model Providers Can Stream Deltas

**Status:** accepted

`ModelProvider::complete` returned a whole `ModelOutput`, and `RunEvent` only
had a final `ModelResult`. Interactive clients had no token-level signal, and
the OpenAI adapter could not use an SSE response.

`ModelProvider` now has `complete_streaming(request, on_delta)`. The default
calls `complete` and emits the full text once, so every provider works without
changes. `Dispatcher` gained `call_model_streaming` with the same default.
`RunEvent::ModelDelta { text }` carries chunks.

The direct execution path runs the model future and drains an unbounded delta
channel concurrently with `tokio::select`, so deltas reach the caller while the
call is still running. Deltas are emitted after `ModelCallStarted` and before
`ModelResult`.

`OpenAiCompatibleModel` overrides `complete_streaming` with an SSE
implementation: it requests `stream: true`, parses `data:` chunks, emits content
deltas and accumulates fragmented tool calls into the final `ModelOutput`.
`capabilities().streaming` is true.

The journaled path uses the same channel. `DispatchOptions` carries an optional
`DeltaSink`; when it is set, the outbox dispatcher calls
`call_model_streaming` and the engine drains the sink with the same
`tokio::select` loop it uses for the direct path. Without a sink the outbox
still calls `call_model` once. A background `outbox` drain has no event
consumer, so it leaves the sink unset.

Consequences: CLI and other clients can render tokens as they arrive, whether
or not the Run is journaled. Streaming is observational: the accepted
`ModelOutput` (and the Effect record) is still the authoritative fact.
A stream that fails mid-way returns a model error and no partial result is
treated as accepted.

Alternatives considered: a `Stream` return type (hard to combine with
`async_trait` and custom dispatchers), provider-specific callbacks outside the
contract (clients would depend on each adapter), and faking deltas after the
fact (not streaming).

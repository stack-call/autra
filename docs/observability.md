# Observability

Autra instruments execution with `tracing` spans. A trace explains a Run; it is
never the authoritative record of one.

## Spans

| Span | Fields | Purpose |
|---|---|---|
| `agent.session.turn` | `session_id`, `turn` | One conversation turn |
| `agent.run` | `agent` | One Run |
| `gen_ai.model.call` | `turn` | One model request |
| `agent.tool.execute` | `tool`, `call_id` | One tool execution |

Run start emits an event with the run id. Completion records the phase; failure
records the error message.

## CLI

The CLI installs an environment-filtered subscriber. Default output is
unchanged because the default level is `warn`.

```bash
RUST_LOG=info cargo run -p autra-cli -- demo
```

## Boundaries

- Durable Events and Stream Events are the execution interfaces. A trace cannot
  restore a Run.
- There is no OpenTelemetry exporter, sampling policy or trace-to-journal
  correlation id yet.
- Tool stdout and model token streams are not spans; they are stream events.

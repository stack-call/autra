# Tracing and observability

**Status:** ready-for-agent

**Source:** design spec §17 (observability, replay and evaluation) and M6.

## Problem Statement

A Run emits stream events to its caller, but there is no structured span tree
for latency, model and tool calls, or failure diagnosis.

## Solution

Instrument the runtime with `tracing` spans: Session turn, Agent Run, model
call and tool execution. The CLI installs a `tracing-subscriber` filtered by
`RUST_LOG`. Traces remain observability data and never authoritative state.

## Implementation Decisions

- Spans: `agent.session.turn`, `agent.run`, `gen_ai.model.call`,
  `agent.tool.execute`; fields include session id, run id, turn, tool name and
  call id.
- A run-start event carries the run id. Completion/failure events record the
  phase.
- The CLI initializes an env-filtered subscriber; default level is `warn`, so
  normal CLI output is unchanged. `RUST_LOG=info` shows spans.
- Durable Events and Stream Events remain the execution interfaces; a trace
  cannot restore a Run.
- No OpenTelemetry exporter or replay format yet.

## Testing Decisions

- A runtime test uses `tracing-test` to assert a run emits the run-start event.
- Existing tests must stay green with no subscriber installed.

## Out of Scope

OTel export, sampling policy, trace-to-journal correlation IDs, replay and
evaluation dashboards.

# Tracing Is Observability, Not State

**Status:** accepted

Structured spans are essential for latency and failure diagnosis, but they are
lossy, sampled and mutable by exporters. Treating a trace as a recovery source
would repeat the error the design calls out for token streams.

We instrument Session turns, Runs, model calls and tool executions with
`tracing`, and the CLI installs an env-filtered subscriber. Traces carry ids
that correlate with runs and effects, but Durable Events remain the only
recovery input.

Consequences: operators get a span tree without changing execution semantics.
There is no OpenTelemetry exporter or sampling policy yet.

Alternatives considered: custom logging (no structure), making OTel the journal
(invalid), and no instrumentation (blind). Revisit when an OTel adapter and
evaluation pipeline are added.

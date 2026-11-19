# Parallel Tool Batches Are Bounded and Ordered

**Status:** accepted

Executing a batch of model-requested tool calls sequentially wastes latency on
independent read-only work. Executing them all at once risks unbounded resource
use and makes cancellation and ordering implicit.

We decided tool batches may run with bounded fan-out through
`max_parallel_tool_calls` (default 1). Calls are dispatched in chunks; the
runtime awaits each chunk. Outcomes are fed back and accepted in model call order even when completion
order differs; stream events preserve completion order. Cancellation is checked before
each dispatch; already-dispatched calls finish and are recorded.

Consequences: the default remains sequential, so shared-workspace safety does
not change. Parallelism is opt-in and does not imply safe writes. Stream events
preserve completion order; accepted history and model feedback follow model
order in LocalEphemeral, and a durable host must record actual commit order. A
tool panic becomes an `is_error` outcome with `outcome_unknown = true`, so
sibling calls complete and are recorded.

Alternatives considered: parallel by default (unsafe for shared workspaces),
unbounded fan-out (no backpressure), and feeding results back in completion
order (non-deterministic history). Revisit when write tools gain version checks
and a per-workspace write policy.

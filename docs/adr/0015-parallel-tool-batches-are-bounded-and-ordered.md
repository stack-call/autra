# Parallel Tool Batches Are Bounded and Ordered

**Status:** accepted

Executing a batch of model-requested tool calls sequentially wastes latency on
independent read-only work. Executing them all at once risks unbounded resource
use and makes cancellation and ordering implicit.

We decided tool batches may run with bounded fan-out through
`max_parallel_tool_calls` (default 1). Calls are dispatched in chunks; the
runtime awaits each chunk. Outcomes are fed back and recorded in the model's
call order even when completion order differs. Cancellation is checked before
each dispatch; already-dispatched calls finish and are recorded.

Consequences: the default remains sequential, so shared-workspace safety does
not change. Parallelism is opt-in and does not imply safe writes. A tool task
panic fails the Run as a protocol error. Completion order is not preserved in
history or model feedback.

Alternatives considered: parallel by default (unsafe for shared workspaces),
unbounded fan-out (no backpressure), and feeding results back in completion
order (non-deterministic history). Revisit when write tools gain version checks
and a per-workspace write policy.

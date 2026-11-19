# 01: Bounded parallel tool calls

**What to build:** A developer can allow a single model response to execute several independent tool calls concurrently, up to an explicit limit. Results still reach the model in the order it requested them, and the default remains sequential.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `max_parallel_tool_calls` is configurable on the builder and overridable per Run.
- [x] A limit of 1 preserves sequential execution and never runs two tools at once.
- [x] A limit of 2 runs two calls concurrently and reports both outcomes.
- [x] Tool outcomes are fed back in model call order even when completion order differs.
- [x] Cancellation before a batch dispatches no tools; cancellation mid-batch records in-flight outcomes and starts no later chunk.
- [x] A tool task panic fails the Run with a protocol error.
- [x] Existing timeout, budget and path-safety tests still pass.
- [x] Docs and roadmap describe the new limit and its shared-write caveat.

# 04: Bounded runs

**What to build:** Every Run is bounded and cancellable. Turn and tool-call budgets are enforced all-or-nothing per model response; a budget or protocol violation fails the Run with a visible message. A shared CancelFlag stops new Effects, lets an in-flight Effect finish and be recorded, and returns a Cancelled outcome rather than an error. An optional per-Effect timeout fails a slow model and turns a slow tool into an error outcome.

**Blocked by:** 02: Tool-using ReAct loop

**Status:** ready-for-agent

- [ ] max_turns defaults to 8 and fails the Run with a clear message when exhausted.
- [ ] A model response requesting more tool calls than the remaining budget executes none and fails the Run.
- [ ] Cancelling before the first Effect prevents the model call and returns a Cancelled outcome.
- [ ] Cancelling during a tool batch lets the in-flight call finish and be recorded, starts no further calls, and returns a Cancelled outcome.
- [ ] Cancellation returns a RunOutcome, not a RunError.
- [ ] Duplicate call IDs, empty call IDs and results for unknown call IDs fail the Run.
- [ ] An optional effect timeout fails a slow model call and converts a slow tool call into an error outcome.
- [ ] Format, clippy with warnings denied, and workspace tests pass.

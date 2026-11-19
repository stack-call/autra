# 02: Tool-using ReAct loop

**What to build:** A caller can register tools, and the model can request one or more of them in a single response. The coordinator executes the calls sequentially in model order, feeds every outcome back before the next model call, and lets the model produce the final answer. Unknown tools and malformed arguments become error outcomes rather than aborting the Run. The event stream reports each tool start and result.

**Blocked by:** 01: Echo agent end-to-end

**Status:** ready-for-agent

- [ ] An Agent with the echo tool completes a FakeModel script of tool call followed by final answer, with one tool call counted.
- [ ] Multiple tool calls in one model response execute in model order, and all outcomes are fed back before the next model call.
- [ ] An unknown tool name produces an error outcome and the Run continues to a final answer.
- [ ] Malformed tool arguments produce an error outcome and the Run continues.
- [ ] The Stepper emits one tool Effect per call and waits for the whole batch before requesting the next model call.
- [ ] Run events include ToolCallStarted and ToolResult with full tool content.
- [ ] Format, clippy with warnings denied, and workspace tests pass.

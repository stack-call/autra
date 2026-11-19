# 01: Echo agent end-to-end

**What to build:** A caller can construct an Agent with instructions and an EchoModel, run one input, and receive the echoed answer. The event callback observes the full Run lifecycle, and `autractl run` prints the same events in a terminal. The Run gets a fresh identity, an authoritative AgentState, a turn count, and completes through the public Stepper seam. AgentState serializes to a stable JSON snapshot.

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] `Agent::builder` accepts instructions and a model; building without a model fails with a typed error.
- [ ] `agent.run("hello").await` returns a Completed outcome with answer `echo: hello` and one turn used.
- [ ] `run_with_options` emits RunStarted, ModelCallStarted, ModelResult and RunCompleted in that order.
- [ ] RunOutcome exposes the authoritative AgentState plus answer, status, turns and tool-call accessors.
- [ ] The ReAct Stepper moves RunStarted to CallModel, and a ModelResult with no tool calls to Completed.
- [ ] `autractl run "hello"` prints the Run events and exits successfully.
- [ ] AgentState serializes to a fixed JSON snapshot.
- [ ] Format, clippy with warnings denied, and workspace tests pass.

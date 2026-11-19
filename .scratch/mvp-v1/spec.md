# MVP v1: Local ReAct Agent

**Status:** ready-for-agent

**Source:** `agent_native_framework_complete_spec_v1.0.md`, especially §28
(Sprint 0) and §22.1 (M1).

## Problem Statement

Rust developers who want to build an agent today either adopt a framework that
drags in infrastructure (a database, a server, Kubernetes) or hand-roll the
ReAct loop, tool dispatch, budgets, cancellation and tests. Autra's design spec
describes the intended execution semantics, but the repository currently holds
only type skeletons and a CLI stub. There is no runnable agent, no
deterministic way to demonstrate the loop, and no executable evidence that the
state machine, budget and cancellation semantics work.

## Solution

Ship a zero-infrastructure local agent as a Rust library plus a small CLI:

- one `Agent` definition built with `Agent::builder`;
- a Sans-I/O ReAct Stepper that decides the next Effect from accepted events;
- a model-provider seam with deterministic `FakeModel` and `EchoModel`;
- read-only workspace tools (`read_file`, `list_files`) plus `echo`;
- sequential Effect execution with turn and tool-call budgets;
- cooperative cancellation;
- a stream-event callback;
- an `autractl` CLI with `run`, `demo`, `tools` and `--help`.

Acceptance is deterministic: `cargo test --workspace` proves the semantics, and
`autractl demo` runs the full `list_files -> read_file -> final answer` loop
without a network, database, sandbox or approval flow.

## User Stories

1. As a Rust developer, I want to construct an Agent from a builder, so that I can go from zero to a runnable agent without wiring a runtime by hand.
2. As a Rust developer, I want to set instructions on the Agent, so that the model receives a stable system prompt on every turn.
3. As a Rust developer, I want to inject a model provider, so that production code and deterministic tests use the same Agent definition.
4. As a Rust developer, I want a scripted FakeModel, so that I can test multi-step ReAct behaviour without a network or API key.
5. As a Rust developer, I want an EchoModel, so that I can smoke-test the loop and the CLI with one line of setup.
6. As a Rust developer, I want to register tools on the builder, so that the model sees a stable tool schema.
7. As a Rust developer, I want a read_file tool confined to a workspace, so that the model cannot read arbitrary host files.
8. As a Rust developer, I want a list_files tool confined to a workspace, so that the model can inspect a project before reading.
9. As a Rust developer, I want an echo tool, so that I can test tool dispatch without touching the filesystem.
10. As a Rust developer, I want absolute paths rejected by workspace tools, so that path handling cannot escape the workspace root.
11. As a Rust developer, I want parent-directory traversal rejected, so that `..` cannot climb out of the workspace.
12. As a Rust developer, I want symlink escapes rejected after canonicalisation, so that a link inside the workspace cannot expose an outside file.
13. As a Rust developer, I want read_file output capped at 32 KiB by default, so that one file cannot flood the Context View.
14. As a Rust developer, I want list_files output capped at 200 entries, so that a large directory cannot flood the Context View.
15. As a Rust developer, I want file reads to report truncation explicitly, so that the model knows the observation is partial.
16. As a model-driven agent, I want a model tool call to become a Tool Effect, so that the Stepper stays free of I/O.
17. As a Rust developer, I want tool calls from one model response executed sequentially, so that tool side effects and event order stay deterministic.
18. As a Rust developer, I want all tool outcomes from one batch fed back before the next model call, so that the model sees a complete observation set.
19. As a Rust developer, I want a failed tool to produce an error outcome that is fed back to the model, so that the model can recover or explain the failure.
20. As a Rust developer, I want an unknown tool name to produce an error outcome, so that one bad model response does not abort the Run.
21. As a Rust developer, I want a final model answer to complete the Run, so that the caller gets one authoritative answer.
22. As a Rust developer, I want text and tool calls in the same model response both recorded, so that process narration is not silently dropped.
23. As a Rust developer, I want an empty model answer to complete the Run with no answer, so that a legal empty response is not treated as a failure.
24. As a Rust developer, I want max_turns enforced, so that a looping agent cannot run forever.
25. As a Rust developer, I want max_tool_calls enforced all-or-nothing per batch, so that budget failures are predictable.
26. As a Rust developer, I want a budget violation to fail the Run with a clear message, so that the cause is visible to the caller.
27. As a Rust developer, I want an event callback during a Run, so that I can stream progress to a CLI, UI or log.
28. As a Rust developer, I want events to carry full tool output, so that library consumers decide their own truncation policy.
29. As a Rust developer, I want the RunOutcome to expose the authoritative AgentState, so that I can inspect history, budget and phase after completion.
30. As a Rust developer, I want RunOutcome accessors for answer, status, turns and tool calls, so that common reads stay ergonomic.
31. As a Rust developer, I want to cancel a Run through a shared CancelFlag, so that another task can stop it without dropping the future.
32. As a Rust developer, I want cancellation checked before each new Effect, so that no new work starts after cancellation.
33. As a Rust developer, I want an in-flight Effect to finish and its outcome recorded after cancellation, so that real observations are not discarded.
34. As a Rust developer, I want a cancelled Run to return a Cancelled outcome rather than an error, so that cancellation is a normal terminal state.
35. As a Rust developer, I want a model-provider failure to fail the Run without retry, so that execution-boundary errors are explicit in v1.
36. As a Rust developer, I want an optional per-Effect timeout, so that a slow model or tool cannot hang a Run when I opt in.
37. As a Rust developer, I want a tool timeout to become an error outcome, so that the model can react to it.
38. As a Rust developer, I want a model timeout to fail the Run, so that a stuck model does not masquerade as a tool problem.
39. As a Rust developer, I want protocol violations (duplicate or empty call IDs, unknown result IDs) to fail the Run, so that silent repair cannot hide bugs.
40. As a Rust developer, I want each Run to get a fresh identity and state, so that repeated and concurrent runs do not share mutable state.
41. As a CLI user, I want `autractl run <prompt>` to execute one Run and print events, so that I can try the library from a terminal.
42. As a CLI user, I want `autractl demo` to run a deterministic tool-using scenario, so that I can verify the full loop without credentials.
43. As a CLI user, I want `autractl tools` to list built-in tools, so that I can see what the model can call.
44. As a CLI user, I want `autractl --help`, so that I can discover the commands.
45. As a Rust developer, I want no database, network, sandbox or API key required for v1, so that the first version runs anywhere Rust runs.
46. As a Rust developer, I want the public AgentStepper trait to remain open, so that a second engine can be added later without a Core rewrite.
47. As a Rust developer, I want the built-in tools to receive an explicit LocalWorkspace, so that no ambient current-directory dependency leaks into library code.
48. As a Rust developer, I want deterministic tests for every semantic branch, so that future refactors cannot silently change execution behaviour.

## Implementation Decisions

### Crate boundaries

- `autra-core` stays Sans-I/O: IDs, AgentState, AgentPhase, AcceptedEvent,
  EventKind, EffectIntent, EffectClass, BudgetSnapshot, CancelFlag,
  ExecutionScope, AgentStepper, Transition, Disposition, StepError. It must not
  depend on Tokio, a database, a sandbox or any harness/runtime crate.
- `autra-harness` contains the ReAct Stepper, the ModelProvider boundary,
  FakeModel, EchoModel, the Tool contract, ToolRegistry, LocalWorkspace and the
  built-in tools.
- `autra-runtime` contains the local execution coordinator and the public Agent
  facade. v1 does not add a separate SDK crate.
- `autra-cli` produces the `autractl` binary and calls the runtime directly.

### Core state machine

- A Run starts in `Ready`; `RunStarted` appends the user input, increments
  `turns_used`, moves to `AwaitingModel` and emits one `CallModel` Effect.
- A `ModelResult` appends an Assistant history item. With no tool calls it moves
  to `Completed`, records the answer and finishes. With tool calls it checks the
  budget, records them as pending, moves to `AwaitingTool` and emits one
  `CallTool` Effect per call, in model order.
- A `ToolResult` appends a Tool history item and removes the matching pending
  call. While calls remain, the Stepper emits no Effect. When the batch is
  complete it checks `max_turns`, increments `turns_used`, moves to
  `AwaitingModel` and emits the next `CallModel`.
- `CancelRequested` moves any non-terminal Run to `Cancelled` and finishes it.
- Budget violations and protocol violations move the Run to `Failed`, record a
  message, and finish with the Failed disposition.
- `max_turns` counts model calls. `max_tool_calls` counts tool calls. Defaults
  are 8 and 8 and are overridable.
- The Stepper is pure with respect to I/O: it receives accepted events and
  returns the next state plus Effect intents.

### Public API

- `Agent::builder(name)` returns a builder with `instructions`, `model`, `tool`,
  `max_turns`, `max_tool_calls` and `effect_timeout`.
- `build()` requires a model and fails with a typed error when it is missing.
- `agent.run(input).await` returns `Result<RunOutcome, RunError>`.
- `agent.run_with_options(input, options, on_event).await` accepts
  `RunOptions { cancel, effect_timeout }` and an `FnMut(&RunEvent)` callback.
- `RunOutcome` holds the authoritative `AgentState` and offers `answer()`,
  `status()`, `turns_used()` and `tool_calls_used()`.
- `RunStatus` is `Completed` or `Cancelled`; failed runs return `RunError`.
- `CancelFlag` is cloneable; `cancel()` is callable from another task.

### Model boundary

- `ModelProvider` is an async trait taking a ModelCallSpec (system
  instructions, history, tool specs) and returning one ModelOutput.
- `FakeModel` replays scripted responses in order and returns a typed error when
  the script is exhausted.
- `EchoModel` returns the last user input as the answer.
- v1 has no token streaming: a ModelResult event is emitted when the whole
  response is accepted.

### Tools and Workspace

- `Tool` is an async trait with `spec()`, `effect_class()` and
  `call(arguments: &str)`. Arguments are the raw JSON string from the model;
  each tool parses and validates its own input.
- `ToolRegistry` is an ordered-by-name map of tool name to tool instance and
  produces the ToolSpec list shown to the model.
- `LocalWorkspace::open(root)` canonicalises the root. Resolution rejects
  absolute paths, parent-directory components and any canonicalised target that
  falls outside the root.
- `read_file` returns UTF-8 text with a default 32 KiB cap and an explicit
  truncation marker. `list_files` lists one directory, sorted, with `/` on
  directories, a 200-entry cap and a truncation marker. `echo` returns its text
  argument.
- Built-in tools are read-only and declare `EffectClass::ReadOnly`.

### Runtime coordinator

- The coordinator owns an ordered queue of AcceptedEvent values and a sequence
  counter. Each Stepper transition is accepted before its Effects are executed.
- Effects are dispatched sequentially: model calls through the provider,
  tool calls through the registry. Every tool failure, including unknown tools,
  bad arguments and timeouts, becomes a ToolOutcome with `is_error = true`.
- A model-provider failure or model timeout fails the Run and returns a typed
  RunError without retry.
- Cancellation is checked before dispatching each new Effect. An Effect already
  in flight is allowed to finish and its outcome is accepted. After that the
  coordinator enqueues CancelRequested, emits no further Effects, and returns a
  Cancelled outcome.
- `RunEvent` variants: RunStarted, ModelCallStarted, ModelResult,
  ToolCallStarted, ToolResult (full content), RunCompleted, RunCancelled and
  RunFailed.
- Each Run gets a fresh RunId and an independent AgentState. The Agent is
  reusable with `&self`; v1 offers no scheduler, session store or cross-run
  concurrency control.

### CLI

- `autractl run <prompt>` builds an Agent with EchoModel and prints events.
- `autractl demo` builds an Agent with a scripted FakeModel, a LocalWorkspace at
  the current directory and the read-only tools; the script lists files, reads
  README.md and then answers.
- `autractl tools` prints built-in tool names and descriptions.
- `autractl --help` prints usage.
- CLI display truncates tool content to 400 characters; the event payload keeps
  the full content.

### Serialization and identity

- AgentState, history items, budget and effect specs derive Serde round-trip
  support.
- IDs are prefixed strings allocated process-uniquely. They are explicitly not
  durable across restarts.

## Testing Decisions

Good tests observe behaviour at public seams. They must not reach into private
helpers, assert on internal call order, or recompute expected values the same
way the implementation does. Expected values come from the spec, from worked
examples, or from fixed literals.

The agreed seams, highest first:

1. `Agent` end-to-end in `autra-runtime`: scripted FakeModel plus real tools,
   asserting outcome, status, budget counters and emitted RunEvents. This is the
   primary seam and covers the coordinator, Stepper and tool dispatch together.
2. `AgentStepper` transitions in `autra-harness`: drive a fixed sequence of
   AcceptedEvents and assert phase, history, pending calls, Effect intents and
   disposition. Covers illegal events, budgets, cancellation and protocol
   violations.
3. `Tool` in `autra-harness`: call built-in tools against a temporary
   LocalWorkspace and assert content, truncation markers, path rejection and
   error outcomes.
4. `AgentState` JSON snapshot in `autra-core`: one fixed state serialized and
   compared to a literal JSON value, so schema changes are visible.

Testing decisions:

- Use FakeModel and temporary directories; no network, no API key and no
  external service.
- Assert user-visible outcomes: final answer, RunStatus, counters, error
  messages, tool output and workspace safety.
- Cover cancellation both before the first Effect and during a tool batch.
- Cover multiple tool calls in one model response and verification that results
  are fed back before the next model call.
- Cover unknown tool, malformed arguments, budget exhaustion, duplicate call
  IDs, empty call IDs and unknown result IDs.
- Keep a CLI smoke test limited to `--help` and `tools`; `demo` is verified by
  the library test that mirrors its script.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
  warnings` and `cargo test --workspace` as the repository gate.

## Out of Scope

- A real model provider (OpenAI-compatible or otherwise); it is the next slice
  and must land behind the existing ModelProvider seam.
- Token-level streaming; v1 emits one ModelResult per accepted response.
- Tool approval, approval binding and the WaitingApproval state.
- Durable execution, SQLite, Outbox, checkpoints, recovery and OutcomeUnknown
  handling.
- Parallel tool execution, tool concurrency limits and result merging.
- ChildRun, Subagents, budgets for nested runs and structured concurrency.
- Sessions, multiple turns per session and any session store.
- Write tools, apply_patch, shell commands, run_tests, git tools and any
  non-read-only EffectClass.
- OpenShell, sandbox backends, network policy and credential handling.
- MCP, Python SDK, HTTP/SSE server, Web UI and multi-session service.
- Multi-run scheduling, backpressure and worker pools.
- Gitee Go CI; the repository keeps the GitHub Actions definition as a
  declarative gate and the same commands run locally.
- Custom Stepper injection through the builder; AgentStepper stays a public
  trait, but ReActStepper is the only implementation wired in v1.

## Further Notes

- The grilling round 3 recommendations were adopted as written because the user
  proceeded to the spec: real provider excluded, full-content events, CancelFlag
  as the cancellation API, reusable Agent with independent run state, 32 KiB and
  200-entry tool caps, raw-string tool arguments, no custom Stepper hook, and
  the `.scratch/mvp-v1` feature slug. Revise here before `/to-tickets` if any of
  those should change.
- Terminology follows `GLOSSARY.md`: Run, Turn, Session, Effect, EffectClass,
  OutcomeUnknown, Tool, Workspace, AcceptedEvent, Stream Event, Context View,
  Execution Host, LocalEphemeral.
- v1 is `LocalEphemeral`: it supports interaction and cancellation, but does not
  promise recovery after process exit. No durable claim may be inferred from
  this spec.
- The next vertical slice after this spec is the real model provider, followed
  by approval and persistence.

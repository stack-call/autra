# Execution Model

## The loop

```text
AcceptedEvent ──▶ AgentStepper ──▶ Transition
                                     │
                        ┌────────────┴────────────┐
                        ▼                         ▼
                    Next State              Effect intents
                        │                         │
                        └──── Coordinator ────────┘
                                  │
                           Effect host executes
                                  │
                        result becomes an AcceptedEvent
```

The Stepper is the only place that decides a transition. The coordinator
orders events, dispatches Effects and accepts outcomes.

## Four message types

| Type | Example | Authoritative? |
|---|---|---|
| Command | SubmitTurn, CancelRun, ApproveEffect | Only its accepted fact |
| Durable Event | EffectIntentRecorded, ToolResultCommitted, RunSuspended | Yes, when a durable host is active |
| Stream Event | token delta, tool stdout, progress | No; may be lost or repeated |
| Telemetry | span, counter, evaluation label | No; never recovery input |

A trace or token stream cannot rebuild business state.

## Current ReAct state machine

`autra-harness::ReActStepper` implements:

```text
Ready
  └─ RunStarted ──▶ AwaitingModel ── CallModel
AwaitingModel
  ├─ ModelResult (no tool calls) ──▶ Completed
  └─ ModelResult (tool calls) ──▶ AwaitingTool ── one CallTool per call
AwaitingTool
  ├─ ToolResult while pending remain ──▶ AwaitingTool
  └─ ToolResult on last pending call ──▶ AwaitingModel ── CallModel
Any non-terminal phase
  └─ CancelRequested ──▶ Cancelled
Violation
  └─ ──▶ Failed
```

Tool results are fed back in model order. The Stepper records an assistant
message with its tool calls, then one Tool history item per outcome.

## Semantics currently implemented

- **Model call**: a provider error or model timeout fails the Run. The failure
  emits `RunFailed` and returns a typed `RunError`.
- **Tool call**: an unknown tool, malformed arguments or tool error becomes an
  `is_error` ToolOutcome and is fed back to the model.
- **Context**: a `ContextBuilder` assembles instructions, history, tools and
  the skill index. An optional character budget truncates content only.
- **Sessions**: an in-memory `Session` seeds accepted history into each turn
  and runs turns sequentially with `&mut self`. Each turn is a fresh Run.
- **Approval**: a tool may declare `requires_approval`; the coordinator resolves
  approvals before dispatch and binds the decision to the call and argument
  digest. Rejection becomes an error outcome.
- **Parallel tools**: with `max_parallel_tool_calls > 1`, a batch runs in
  bounded chunks. `ToolResult` stream events follow completion order; accepted
  history and model feedback follow model order. A panicking tool becomes an
  unknown error outcome.
- **Tool timeout**: the outcome is `is_error = true` **and**
  `outcome_unknown = true`. The future was dropped, but the external side
  effect may continue. See [`ADR-0014`](adr/0014-tool-timeout-is-outcome-unknown.md).
- **Budget**: `max_turns` and `max_tool_calls` default to 8. A tool batch is
  accepted all-or-nothing; exceeding the budget fails the Run before any tool
  in that batch executes.
- **Cancellation**: checked before every Effect. An in-flight Effect finishes
  and its outcome is recorded. A cancellation that arrives while a final model
  call is in flight does not turn an already-completed Run into Cancelled.
- **Protocol violations**: empty or duplicate call ids and results for unknown
  call ids move the Run to Failed.
- **Empty answer**: a model response with no text and no tool calls completes
  the Run with no answer.

## Invariants

The full list is in the spec §3.3. The ones exercised by the current code:

- INV-01: accepted state changes are ordered.
- INV-03: a lost tool result is never treated as "not executed".
- INV-05: budgets are bounded; a child cannot silently amplify them.
- INV-06: cancellation does not undo external effects.
- INV-07: context compaction cannot delete execution facts.
- INV-08: no implicit equivalence between execution hosts.

# Transition Proposals Are Validated Centrally

**Status:** accepted.

## Context

`AgentStepper::transition` returns a `TransitionProposal` containing the next
`AgentState`. The coordinator committed that state with a version CAS, but it
did not verify the rest of the state contract. A custom stepper could:

- change `run_id`, `parent_run_id`, `session_id` or `turn_id`;
- raise `max_turns` or `max_tool_calls`;
- lower usage counters;
- move the phase graph backwards;
- switch `engine_id` mid-run.

The default ReAct stepper happens to respect these rules, but the boundary is
public and third-party engines must not be trusted by convention.

## Decision

The Runtime validates every proposal before recording Effects or committing:

- Run, parent, Session and Turn identity are immutable within a Run.
- `state_version` must advance by exactly one from the expected version.
- Budget limits are immutable; usage counters are monotonic and may not exceed
  their limits.
- An initialized engine cannot be replaced by another engine.
- Phase transitions follow a fixed graph:
  `Ready -> {AwaitingModel, AwaitingTool, Completed, Failed, Cancelled}`,
  `AwaitingModel -> {AwaitingModel, AwaitingTool, Completed, Failed, Cancelled}`,
  `AwaitingTool -> {AwaitingTool, AwaitingModel, Completed, Failed, Cancelled}`,
  terminal phases only to themselves.
- `Failed` must carry `last_error`.
- The accepted event must be legal for the current phase: `RunStarted` only in
  `Ready`, `ModelResult` only in `AwaitingModel`, `ToolResult` only in
  `AwaitingTool`, `ResumeRequested` only in `AwaitingModel`, and
  `CancelRequested` only in a non-terminal phase.
- Disposition must match the next phase: `Finished` ends completed or cancelled,
  `Failed` ends failed, and `Continue` stays non-terminal.
- Intent shape: at most one model call per transition; a model intent requires
  `AwaitingModel`; tool intents require `AwaitingTool`; tool call ids are
  non-empty and unique; a cancelled transition proposes no new intents.
- Budget accounting is checked against the proposal: the increase in
  `tool_calls_used` must equal the number of tool intents.

The same identity, budget and `Ready` phase rules are checked after
`AgentStepper::initialize`, so engine initialisation cannot rewrite the Run's
identity or budget. A rejected proposal or initialisation fails the Run before
any Effect is dispatched.

## Consequences

- A custom engine cannot silently widen its authority or corrupt recovery.
- New phase transitions become a conscious change to the validator.
- The validator is a runtime concern; Core keeps the transition protocol and
  Harness keeps the behaviour.
- The validator is not a type-state system: it checks values at commit time, not
  at compile time. This keeps `AgentState` serialisable and engine-agnostic.

See ADR-0028 (transition is a proposal) and ADR-0040 (atomic transition
commit).

# Autra

A Rust-native agent framework. This glossary fixes the vocabulary used across
the spec, tickets and code. It is a glossary, not a spec.

## Execution

**Run**:
One logical execution working toward a task goal, from start to completion,
failure or cancellation.
_Avoid_: Job, task, invocation

**Turn**:
One interaction boundary between a user input and an agent response or wait.
_Avoid_: Round, message

**Session**:
A semantic container for a sequence of turns with one user or caller.
_Avoid_: Conversation, thread, chat

**RunAttempt**:
One physical execution attempt of a Run by a host.
_Avoid_: Worker, task

**ChildRun**:
A Run created by a parent Run, constrained by the parent's scope and budget.
_Avoid_: Subagent, subtask

**Run Snapshot**:
A point-in-time read model of a Run (phase, budget, answer, last event,
effect count) for clients that cannot hold the state.
_Avoid_: State dump, status

**Stepper**:
The Sans-I/O state machine that turns accepted events into the next execution
intent.
_Avoid_: Engine, loop

**Run State**:
The authoritative state of one Run. The current Rust type is `AgentState`; it
carries optional Session and Turn references.
_Avoid_: Context, history

## Effects and tools

**Effect**:
One observable I/O intent and its accepted outcome.
_Avoid_: Action, operation, side effect

**EffectClass**:
The declared category of a tool effect: ReadOnly, Idempotent, Reconcileable,
NonIdempotent or Unknown.
_Avoid_: Retry policy

**OutcomeUnknown**:
The state where an effect may have executed but its result was not recorded.
It is never treated as "not executed".
_Avoid_: Failed, lost

**Tool**:
A capability the model may request, described by a schema and an execution
contract.
_Avoid_: Function, command, action

**Approval**:
Authorization for one specific effect, bound to its parameters, actor, policy
revision and expiry.
_Avoid_: Permission, consent

**Approval Record**:
A persisted approval decision for one effect, reusable only when the argument
digest and policy revision match and it has not expired.
_Avoid_: Permission, token

**Policy**:
A rule set that can only narrow the host baseline for a tool call; its
decision is Allow, Ask or Deny.
_Avoid_: Permission system, ACL

**Reconciliation**:
Retrying `OutcomeUnknown` effects only when their class makes a retry safe.
_Avoid_: Retry, recovery

**Workspace**:
The file environment a tool operates on, identified separately from the Run's
execution state.
_Avoid_: Sandbox

## Messages and state

**AcceptedEvent**:
An event the coordinator has ordered and accepted as authoritative input to
the stepper.
_Avoid_: Message, input

**Durable Event**:
An accepted event recorded by a durable host as the basis for recovery.
_Avoid_: Log entry, trace

**Session Store**:
Persistence for session turns and conversation history, separate from the
execution journal.
_Avoid_: Journal, conversation log

**Turn Record**:
The stored input, status, answer, error and accepted history for one Session
turn.
_Avoid_: Message, transcript

**Stream Event**:
A non-authoritative real-time event, such as a token delta or progress update,
that may be lost or duplicated.
_Avoid_: Durable event

**Context View**:
The versioned model-input view built for one model call. It is not the
authoritative journal.
_Avoid_: Prompt, history

**Journal**:
The authoritative ordered record owned by an execution host.
_Avoid_: Log, database

## Hosts

**Execution Host**:
The component that owns effect acceptance and recovery guarantees for a Run.
_Avoid_: Runtime, server

**LocalEphemeral**:
The default execution host: in-process state, with no recovery after process
exit.
_Avoid_: Dev mode

**NativeDurable**:
The optional SQLite-backed execution host that recovers at recorded
boundaries.
_Avoid_: Durable mode

**RestateHosted**:
The optional execution host that delegates journaling and durable waits to
Restate.
_Avoid_: Restate backend

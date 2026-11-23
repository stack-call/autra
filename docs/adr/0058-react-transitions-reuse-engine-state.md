# ReAct Transitions Reuse Engine State

**Status:** accepted

## Problem

`ReActStepper` stored the full conversation history inside
`AgentState.engine_state` as JSON. Every transition decoded it, cloned
`ReactState`, cloned the entire `AgentState` (including the previous JSON
payload), and serialized the history again. The per-transition cost grew with
the whole history, so a long Run accumulated roughly quadratic copying and
serialization work.

## Decision

1. `fork_state` builds the next `AgentState` by copying small scalar fields and
   replaces `engine_state` instead of cloning the previous JSON payload.
2. `ReActStepper` keeps one decoded `ReactState` keyed by
   `(run_id, state_version)`. A transition takes the cached value, mutates it,
   serializes it once into the next state, and keeps the same value for the next
   version.
3. `store_react_state` now takes `&ReactState`; callers no longer clone the
   history just to serialize it.
4. A cache miss (resume, new Run, version mismatch after a rejected proposal)
   falls back to decoding from `AgentState`.

## Consequences

A model/tool transition no longer performs a full `AgentState` clone, a second
deep clone of the history, or a JSON decode. It still serializes the history
once because the history remains the durable representation inside
`engine_state`.

The cache is per stepper, and the Agent creates one stepper per Run, so it does
not mix histories across Runs. Holding one decoded state per active Run is the
memory cost. `ResumeRequested` and `CancelRequested` still clone `AgentState`
because they preserve the existing engine payload.

This is an incremental fix, not a structural one. Removing the remaining
linear per-transition copy requires a typed engine-state field on `AgentState`
or structural sharing of history, which would couple Core to a specific engine
and needs a separate ADR.

## Alternatives

- Store engine state as a typed variant in Core: rejected for now; it breaks the
  opaque `engine_id` boundary.
- Use `Arc<Vec<HistoryItem>>`: rejected; serialization and mutation semantics
  become more complex without removing the durable JSON.
- Add no cache and only remove `react.clone()`: cheaper to implement but leaves
  a JSON decode on every transition.

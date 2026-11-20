# Sessions and multiple turns

**Status:** ready-for-agent

**Source:** design spec §11.2 (Session/Turn/Run relationship) and the M3
milestone.

## Problem Statement

`Agent::run` is single-turn. A caller who wants a conversation must carry the
message history and rebuild the Agent state for every input, and there is no
explicit Session or Turn identity.

## Solution

Add an in-memory `Session` that owns an Agent and the accepted history. Each
turn starts a fresh Run with a fresh budget and Run id, seeded with the
Session history, then stores the resulting history back into the Session.
The CLI gains a line-oriented `autractl chat` command.

## Implementation Decisions

- `Session` owns the Agent and `Vec<HistoryItem>`. `run_turn` takes `&mut self`,
  enforcing the spec's single-writer rule for one Session.
- Each turn is one Run with its own `RunId`, `TurnId`, budgets and cancellation.
  The history and optional Session/Turn ids are seeded into `AgentState` before
  RunStarted.
- `SessionId` is fresh per Session; `turn_index` increments before each turn.
- On Completed or Cancelled, the Session stores the outcome history. On a
  `RunError`, the previous history is kept unchanged.
- `Agent::run_with_history` is the seam; `run` and `run_with_options` remain
  the single-turn API.
- `autractl chat [--model <name>]` reads non-empty lines from stdin, runs one
  turn per line and prints the same events as `run`.
- No persistence, cross-process session store, concurrent turns or server.

## Testing Decisions

- Runtime seam: two turns; the second model request contains the first turn's
  user and assistant messages; the Session history has four items.
- Session identity: two Sessions get different ids; turn index advances.
- Cancelled turn updates history with the accepted partial history.
- CLI test: pipe two lines into `autractl chat` and see two echoed answers.

## Out of Scope

Durable sessions, server-side session stores, concurrent turns in one Session,
approval persistence and session-level policy.

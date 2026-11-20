# Session Store Is Separate from the Execution Journal

**Status:** accepted

`Session` kept its turn history in memory. `chat --journal` persisted Run facts
but lost the conversation when the process exited, and there was no record of
turn inputs, answers or failures to restore.

The session contract now lives in `autra-execution`:

- `TurnRecord` records turn id, index, run id, input, status, answer, error and
  the accepted history after the turn;
- `SessionRecord` holds ordered turns and returns the last turn's history;
- `SessionStore` creates a session, loads it and saves (upserts) a turn.

`autra-durable-sqlite` implements `SqliteSessionStore` with `sessions` and
`session_turns` tables. `Session::open` restores a named session; `Session`
saves Completed, Cancelled and Failed turns after each run. `chat --journal
--session <id>` restores or creates that session and prints its id.

Consequences: session history survives process exits and is independent of Run
recovery. The two stores may share a SQLite file but never share a state
machine: the journal owns authoritative Run facts, the session store owns
conversation and turn metadata. A turn is written after it finishes, so a crash
mid-turn leaves no partial turn record. Restoring is explicit; there is no
`SessionManager` or multi-client snapshot protocol yet.

Alternatives considered: storing turns in the execution journal (couples
conversation metadata to Run recovery and the state machine), one `Session`
per database (no multi-session support), and reconstructing history from the
journal's events (it cannot see turns that never committed and would mix the
two truths).

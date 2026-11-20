# 01: Runtime tracing

**What to build:** A developer can observe Sessions, Runs, model calls and tool calls as structured spans, while keeping traces separate from authoritative execution state.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `agent.run` and `gen_ai.model.call` spans are emitted with run/turn fields.
- [x] `agent.tool.execute` spans carry tool and call-id fields.
- [x] `agent.session.turn` spans carry the session id.
- [x] The CLI installs an env-filtered subscriber; default output is unchanged.
- [x] A tracing test asserts the run-start event; existing tests stay green.
- [x] Docs and an ADR state that traces are not execution state.

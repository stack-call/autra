# Harness v1 Acceptance

This page maps the design spec's "first complete harness version" (§0.4) to the
repository. It is a scope statement, not a claim that the full design is
implemented.

## Checklist

| Capability | Implementation | Boundary |
|---|---|---|
| Context management | `ContextBuilder`, `SimpleContextBuilder`, character budget | Character proxy; no tokenizer or summarization |
| Skills | `SkillRegistry`, `load_skill` | Loading does not grant permissions |
| Planning | `update_plan`, `Agent::plan()` | In-memory per Agent; no planner engine |
| Restricted Subagents | `spawn_subagent`, named child Agents | Tool-backed; no runtime ChildRun cancellation/budget slice |
| Workspace Tools | `read_file`, `list_files`, `write_file` | `write_file` requires approval; no shell/apply_patch |
| Tracing | `agent.session.turn`, `agent.run`, `gen_ai.model.call`, `agent.tool.execute` | No OTel exporter |
| Basic Evaluation | `autra-eval` task report | Outcome-based; not runtime fault evidence |

## Additional harness capabilities

- ReAct Stepper and local execution host.
- Sequential and bounded parallel tool batches.
- Budgets, cooperative cancellation and optional Effect timeouts.
- Approval bound to Run, call, tool and argument digest.
- In-memory Sessions with multiple turns.
- OpenAI-compatible non-streaming provider.
- Deterministic FakeModel/EchoModel test paths.

## Boundaries

- Execution host is `LocalEphemeral`: no process-exit recovery.
- No durable journal, SQLite, Restate, OpenShell, MCP, Python SDK, HTTP server
  or multi-session service.
- No shell command tool or OS sandbox; `LocalWorkspace` is path validation only.
- Sessions and plans are in memory; approvals have no expiry or actor identity.
- Runs carry optional Session/Turn ids; SessionManager, TurnRecord and a
  SessionStore are not implemented.
- Subagents do not propagate cancellation or share a budget slice.
- The CLI's `run`/`chat` commands do not register workspace tools; `demo` uses
  the read-only workspace tools and `tools` lists the built-in registry.

## Evidence

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p autra-cli -- demo
printf 'hello\nagain\n' | cargo run -p autra-cli -- chat
RUST_LOG=info cargo run -p autra-cli -- demo
```

The workspace suite covers Stepper transitions, tool and approval contracts,
parallel ordering/cancellation, sessions, context and skills, planning,
subagents, tracing and evaluation.

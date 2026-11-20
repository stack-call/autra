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
| Restricted Subagents | `spawn_subagent`, named child Agents | Runtime ChildRun identity, inherited cancellation and an independent budget; automatic parent budget settlement and parent event forwarding pending |
| Workspace Tools | `read_file`, `list_files`, `search_text`, `git_status`, `git_diff`, `edit_file`, `write_file`, `run_command` | Writes and commands require approval; `run_command` is not a sandbox |
| Tracing | `agent.session.turn`, `agent.run`, `gen_ai.model.call`, `agent.tool.execute` | No OTel exporter |
| Basic Evaluation | `autra-eval` task report | Outcome-based; not runtime fault evidence |

## Five Harness subsystems

| Subsystem | Responsibility |
|---|---|
| Agent Engine | ReAct and future Planning/Workflow strategies; emits uniform EffectIntents |
| Context System | Selection, retrieval, compaction, budget, sources; assembles model input |
| Capability System | Tools, Skills, MCP discovery and visibility |
| Subagent System | Child Agent definitions, delegation, result integration |
| Model System | Provider adapters, streaming, structured output, capability declaration |

Composition rules: Skills do not grant permissions; Context reads but does not own
the journal; Subagents reuse Run/ChildRun semantics; provider-specific fields stay in
adapters. Memory, MCP and full Planning/Workflow remain future work.

## Additional harness capabilities

- ReAct Stepper and local execution host.
- Sequential and bounded parallel tool batches.
- Budgets, cooperative cancellation and optional Effect timeouts.
- Approval bound to Run, call, tool and argument digest.
- In-memory Sessions with multiple turns.
- OpenAI-compatible provider with SSE streaming and non-streaming completions.
- Deterministic FakeModel/EchoModel test paths.

## Boundaries

- Execution host is `LocalEphemeral` by default; with an opt-in SQLite journal, `Agent::resume` continues committed checkpoints.
- No Restate, OpenShell, MCP, Python SDK, HTTP server, multi-session service or
  plugin manager. The SQLite journal and SessionStore are opt-in adapters.
- No OS sandbox. `run_command` runs an unsandboxed local shell in the workspace root; `LocalWorkspace` is path validation only.
- Sessions and plans are in memory. Approvals persist with a policy revision, a
  claimed actor and an expiry timestamp; actor identity is not verified.
- Runs carry optional Session/Turn ids; `SessionStore` and `TurnRecord` are implemented (in-memory and SQLite), but `SessionManager` and a multi-client snapshot protocol are not.
- Subagents run as ChildRuns with inherited cancellation and an independent
  budget; parent budget settlement and parent event forwarding are not automatic.
- The CLI's `run`/`chat` commands register the workspace tools and require
  approval for `write_file` and `run_command` unless `--yes` is passed.

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

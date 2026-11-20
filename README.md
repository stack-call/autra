# Autra

> **愿景 / Vision**
>
> Autra 希望为 Agent 提供一套统一、可组合、可验证的构建与执行模型，让智能行为能够自由演进，而执行控制、权限、安全和可靠性始终保持明确的语义。
>
> Autra aims to give agents a unified, composable and verifiable model for
> building and execution, so intelligent behavior can evolve freely while
> execution control, permissions, security and reliability always keep explicit
> semantics.

A Rust-native agent framework: composable intelligence, structured execution,
explicit effects, optional infrastructure.

> **Status: Harness v1 implemented.** Local ReAct agent, sessions, approval,
> context and a character budget, skills, planning, bounded parallel tools,
> restricted subagents, tracing, basic evaluation and an OpenAI-compatible
> provider. The broader design spec remains a proposal beyond this slice. See
> [`agent_native_framework_complete_spec_v1.0.md`](./agent_native_framework_complete_spec_v1.0.md)
> and [`.scratch/mvp-v1/spec.md`](./.scratch/mvp-v1/spec.md).

## Layout

| Path | Role | Spec |
|---|---|---|
| `crates/autra-core` | Sans-I/O stepper: IDs, state, accepted events, effect intents, scopes | §6 |
| `crates/autra-execution` | Execution contracts: tool/model/journal/approval/host/sandbox, shared by harness and hosts | §31 |
| `crates/autra` | Product facade: `Agent`, `AgentBuilder`, `Session`, `Subagent` assembly | §14 |
| `crates/autra-harness` | ReAct engine, context, skills, planning, concrete tools; optional `process` tools | §7–§9 |
| `crates/autra-runtime` | Local engine: coordinator, effect dispatch, outbox, resume | §10–§12 |
| `crates/autra-cli` | `autractl`: local runs, streaming, approvals | §14.4 |
| `crates/autra-openai` | OpenAI-compatible model provider, SSE streaming and non-streaming | — |
| `crates/autra-eval` | Task-level outcome evaluation | §17.4 |
| `crates/autra-testkit` | Shared protocol contract checks | §31.4 |
| `crates/autra-durable-sqlite` | SQLite Run / Event / Effect / Approval journal and SessionStore | §15 |

Dependency direction is enforced by Cargo and by
`crates/autra-testkit/tests/architecture.rs`: `autra-core` depends on nothing
Autra-specific; `autra-execution` depends on core only; harness and runtime
depend on core + execution and not on each other; `autra` assembles harness and
runtime; the CLI and eval use the facade, not runtime directly.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p autra-cli -- run "hello"
cargo run -p autra-cli -- chat
cargo run -p autra-cli -- demo
cargo run -p autra-cli -- tools
```

`autra-core` is dependency-light (serde, thiserror). `autra-execution` adds
async-trait, serde_json, sha2 and thiserror. `autra-harness` adds async-trait,
serde and similar; its `process` feature is the only Tokio dependency and is
opt-in for `run_command`/git tools. `autra-runtime`, `autra` and `autra-cli`
use Tokio. The default local path has no database or sandbox dependency;
`autra-openai` adds an optional HTTP client for real providers.

## CLI

After building, the binary is `autractl` (or prefix with
`cargo run -p autra-cli --`).

- `autractl run [options] <prompt>` — run one turn as a workspace agent and print its events.
- `autractl chat [options]` — one turn per stdin line, keeping session history.
- `autractl resume --journal <path> --model <name> <run-id>` — continue a checkpointed run.
- `autractl snapshot --journal <path> <run-id>` — print the current Run snapshot as JSON.
- `autractl approve|reject --journal <path> <run-id> <effect-id>` — record an approval decision for a pending effect.
- `autractl effects --journal <path> [--unknown] <run-id>` — list a Run's effects.
- `autractl reconcile --journal <path> [--model <name>] <run-id>` — retry safe `OutcomeUnknown` effects.
- `autractl outbox --journal <path> --model <name>` — drain due outbox rows, reclaiming expired leases.
- `autractl demo` — deterministic list-files, read-README demo.
- `autractl tools` — list the built-in tools.

`run` and `chat` work inside one workspace and expose these tools:

| Tool | Purpose | Approval |
|---|---|---|
| `list_files` | list one directory | no |
| `read_file` | read a UTF-8 file | no |
| `search_text` | search files and return `path:line` matches | no |
| `git_status` | show the workspace repository status | no |
| `git_diff` | show unstaged or staged changes, optionally for one path | no |
| `edit_file` | replace an exact string; ambiguous matches are rejected | yes |
| `write_file` | create or replace a file, including parent directories | yes |
| `run_command` | run a shell command in the workspace root | yes |

Before asking for approval, the CLI prints the tool's preview: the command, a
unified diff for edits and overwrites, or the rejection reason for an ambiguous
edit.

| Flag | Meaning |
|---|---|
| `--workspace <dir>`, `-C <dir>` | workspace root (default `.`); file tools stay inside it |
| `--model <name>`, `-m <name>` | OpenAI-compatible model; omit for the local echo model |
| `--local` | use the built-in echo model explicitly (needed for `resume`/`outbox` without a provider) |
| `--journal <path>`, `-j <path>` | persist run state, events and effects to a SQLite journal |
| `--session <id>` | restore or create a named chat session (requires `--journal`) |
| `--policy <path>` | JSON tool/command policy (`ask_tools`, `deny_tools`, `deny_commands`; Allow is the default) |
| `--actor <name>` | operator identity recorded on approval requests |
| `--yes`, `-y` | approve `write_file`, `edit_file` and `run_command` without prompting |
| `--read-only` | expose only `list_files`, `read_file` and `search_text` |

```powershell
cargo run -p autra-cli -- run --workspace ..\my-project --yes --model gpt-4o-mini "Fix the failing test and run it again"
cargo run -p autra-cli -- run --journal .autra.db --model gpt-4o-mini "Inspect the failing test"
cargo run -p autra-cli -- resume --journal .autra.db --model gpt-4o-mini <run-id>
```

`resume` requires `--journal` and an explicit `--model` or `--local`: the journal
stores Run state and effects, not provider configuration. Model calls emit
`RunEvent::ModelDelta` chunks; the OpenAI adapter streams SSE responses. A run interrupted while a tool
or model call was in flight never retries that call blindly: an in-flight
read-only or idempotent tool is retried, any other tool becomes
`OutcomeUnknown`, and an interrupted model call is retired and re-issued.
`outbox` drains rows whose claim lease expired; a record that already resolved
is only cleaned up.

> `run_command` is a local unsandboxed shell, not an OS sandbox. Prefer
> `--read-only` for inspection, and avoid `--yes` on repositories you do not
> trust. A policy file can deny tools or command patterns even under `--yes`;
> see [tool security](docs/tool-security.md).

## Real model provider

`autractl run --model <name> <prompt>` uses an OpenAI-compatible chat
completions endpoint.

- `OPENAI_API_KEY` (required)
- `OPENAI_BASE_URL` (optional, default `https://api.openai.com/v1`)

```bash
OPENAI_API_KEY=... cargo run -p autra-cli -- run --model gpt-4o-mini "Explain this crate"
# or, inside a project:
cargo run -p autra-cli -- run --model gpt-4o-mini "Add a --verbose flag to the CLI"
```

Set `OPENAI_BASE_URL` to target a compatible provider such as DeepSeek,
Moonshot, vLLM, Ollama or LM Studio.

## Documentation

- [Philosophy](docs/philosophy.md) · [Architecture](docs/architecture.md) · [Concepts](docs/concepts.md)
- [Execution model](docs/execution-model.md) · [Structured concurrency](docs/structured-concurrency.md)
- [Context and memory](docs/context-and-memory.md) · [Tool security](docs/tool-security.md)
- [Durability](docs/durability.md) · [Model providers](docs/model-providers.md)
- [Testing and evaluation](docs/testing-and-evaluation.md) · [Observability](docs/observability.md)
- [Evaluation](docs/evaluation.md) · [Harness v1 acceptance](docs/harness.md) · [Extensions](docs/extensions.md)
- [Plugins and capabilities (design only)](docs/plugins.md)
- [Extension architecture v1.0 (design proposal)](docs/extension-architecture-v1.md)
- [Roadmap](docs/roadmap.md)
- [Architecture decisions](docs/adr/) · [OpenShell](docs/integrations/openshell.md) · [Restate](docs/integrations/restate.md)

## Architecture boundaries

Session and Turn are product-level concepts. `autra-core` carries optional
`SessionId` and `TurnId` references on the Run state; Session lifecycle and
history live in `autra-runtime`; context construction lives in
`autra-harness`. See
[ADR-0022](docs/adr/0022-session-and-turn-are-domain-concepts.md) and
[docs/architecture.md](docs/architecture.md).

## Goals

1. **Semantic consistency** — one execution protocol for every engine.
2. **Composability** — Context, Skills, Tools and infrastructure combine independently.
3. **Verifiable execution** — contracts and tests for cancellation, concurrency, approval and effects.
4. **Optional infrastructure** — local by default; durability, sandbox and remote execution are opt-in.
5. **Simple API, powerful runtime** — a small builder for common work, explicit controls for advanced use.

See [docs/philosophy.md](docs/philosophy.md) for the evidence and current gaps.

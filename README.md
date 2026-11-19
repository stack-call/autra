# Autra

A Rust-native agent framework: composable intelligence, structured execution,
explicit effects, optional infrastructure.

> **Status: design proposal.** The API, CLI, schema and milestones described in
> the spec are proposed, not implemented. See
> [`agent_native_framework_complete_spec_v1.0.md`](./agent_native_framework_complete_spec_v1.0.md).

## Layout

| Path | Role | Spec |
|---|---|---|
| `crates/autra-core` | Sans-I/O stepper: IDs, state, accepted events, effect intents, scopes | §6 |
| `crates/autra-harness` | ReAct engine, model provider boundary, context, tools | §7–§9 |
| `crates/autra-runtime` | Tokio local host: effect dispatch, sessions, concurrency, cancellation | §10–§12 |
| `crates/autra-cli` | `autractl`: local runs, streaming, approvals | §14.4 |

Dependency direction is enforced by Cargo: `autra-core` must not depend on
Tokio, a database, a sandbox backend, or any harness/runtime crate.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

No dependencies are declared yet. Tokio arrives with the local host
(`RUNTIME-001`), SQLite with the durable host (`DURABLE-002`).

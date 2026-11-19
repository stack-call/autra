# Autra

A Rust-native agent framework: composable intelligence, structured execution,
explicit effects, optional infrastructure.

> **Status: early implementation.** MVP v1 is in progress. The design spec
> remains a proposal beyond the implemented slice. See
> [`agent_native_framework_complete_spec_v1.0.md`](./agent_native_framework_complete_spec_v1.0.md)
> and [`.scratch/mvp-v1/spec.md`](./.scratch/mvp-v1/spec.md).

## Layout

| Path | Role | Spec |
|---|---|---|
| `crates/autra-core` | Sans-I/O stepper: IDs, state, accepted events, effect intents, scopes | §6 |
| `crates/autra-harness` | ReAct engine, model provider boundary, context, tools | §7–§9 |
| `crates/autra-runtime` | Local host: effect dispatch, sessions, concurrency, cancellation | §10–§12 |
| `crates/autra-cli` | `autractl`: local runs, streaming, approvals | §14.4 |

Dependency direction is enforced by Cargo: `autra-core` must not depend on
Tokio, a database, a sandbox backend, or any harness/runtime crate.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p autra-cli -- run "hello"
```

`autra-core` is dependency-light (serde, thiserror). `autra-harness` adds
async-trait, serde, serde_json and thiserror; its tests use Tokio but the
library does not. `autra-runtime` and `autra-cli` use Tokio. There is no
database, network or sandbox dependency yet.

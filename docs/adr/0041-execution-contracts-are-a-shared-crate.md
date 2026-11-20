# Execution Contracts Are a Shared Crate

**Status:** accepted

`autra-runtime` mixed three kinds of responsibility: contracts that adapters
and the harness need (`ExecutionHost`, `EffectJournal`, `Approver`, `Tool`,
`ModelProvider`), the local execution mechanism, and the public `Agent` API.
Because harness depended on core only and runtime depended on harness, a host
adapter had to depend on the whole runtime (and Tokio) just to implement the
journal contract.

`autra-execution` now holds the execution contracts and depends on
`autra-core` only:

- `Tool`, `ToolRegistry`, `ModelProvider` and their descriptors/errors;
- `EffectJournal`, `OutboxEntry`, `JournalError` and the default claim lease;
- `Approver`, `ApprovalRequest` and the argument/intent digests;
- `ObserverHook`, `ExecutionHost`, `HostCapabilities` and `SandboxBackend`.

`autra-harness` implements the tool and model contracts; the concrete tools
and deterministic models stay there. `autra-runtime` implements dispatch,
journals and coordination. `autra-durable-sqlite` implements
`EffectJournal` from execution, so it no longer depends on runtime at all.
The old module paths remain as re-exports, so callers see no change.

Consequences: a new host or adapter can depend on core plus execution without
pulling in the harness, Tokio or the Agent facade. The dependency rule is now
enforceable: execution must not depend on harness or runtime. The facade split
is not complete: runtime still depends on harness for `ReActStepper` and
context assembly, and `Agent`/`AgentBuilder` still live in runtime. The next
step is an `autra` facade crate that owns the product API and assembles
harness plus runtime; only then can runtime drop its harness dependency.

Alternatives considered: moving contracts into `autra-core` (it would stop
being a small sans-I/O protocol and gain tool/journal concepts), keeping the
contracts in runtime (the current coupling), and creating the facade first
without a stable contract layer (the facade would still re-export runtime
types and the dependency direction would stay wrong).

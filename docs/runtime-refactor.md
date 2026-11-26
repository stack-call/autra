# Runtime refactor

Baseline: `5a011a2d674cd8787bcdae860465e5df81291ed5` (`mvp-v1`).

Implementation in progress. This branch preserves the Sans-I/O Core, Stepper,
Harness behaviour, Dispatcher, ToolRegistry, TransitionCommitter and Journal.

1. Unify file/process resources behind ExecutionEnvironment and EnvironmentProvider;
   retain LocalWorkspace constructors and adapt SandboxBackend.
2. Give EffectExecutor owned, explicit execution dependencies.
3. Replace pooled result delivery with a durable Result Inbox in the Journal,
   including atomic publication and consumption, full ModelOutput and deduplication.
4. Add environment affinity and quotas, reserve capacity before claiming Effects,
   and fence stale Effect and Coordinator writes.
5. Validate recovery, cancellation, approval, competing executors, schema migration
   and existing workspace tests. Save each completed stage to this remote branch.

Invariant: commit state and outbox before dispatch; persist results before acceptance;
never automatically repeat an interrupted non-idempotent Effect.

Environment resource capabilities extend ADR-0025; result consumption extends
ADR-0040's atomic boundary while preserving ADR-0039 and ADR-0053.

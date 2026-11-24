# Engine State Uses Snapshots and Patches

**Status:** proposed

## Context

`AgentState.engine_state` is opaque JSON owned by the Stepper. ReAct stores its
full conversation history there. Every transition currently writes the whole
`AgentState` to the journal, so a long Run writes an ever-growing history blob
on every step.

There is no deployed journal data to migrate. The new design can define the
storage format directly instead of carrying a legacy full-state path.

## Decision

Keep the in-memory `AgentState` unchanged. Persist `engine_state` separately as:

```text
latest snapshot + contiguous JSON Patch rows
```

### 1. Transition proposal carries an optional patch

```rust
pub struct EngineStateDelta {
    pub base_version: u64,
    pub patch: serde_json::Value, // RFC 6902
}

pub struct TransitionProposal {
    pub next: AgentState,
    pub intents: Vec<EffectIntent>,
    pub disposition: Disposition,
    /// None means the Stepper cannot produce a safe patch.
    pub engine_delta: Option<EngineStateDelta>,
}
```

A Stepper that returns `None` keeps the current behavior: the journal writes a
full snapshot. Custom Steppers do not need to implement deltas.

### 2. ReAct emits append-oriented patches

ReAct changes its state in three ways:

| Event | Patch |
|---|---|
| `RunStarted` | append `User` to `/data/history` |
| `ModelResult` | append `Assistant`, replace `pending_tool_calls` |
| `ToolResult` | append `Tool`, replace `pending_tool_calls` |
| `ResumeRequested` / `CancelRequested` | usually no engine change |

Example:

```json
[
  { "op": "add", "path": "/data/history/-", "value": { "User": "hello" } },
  { "op": "replace", "path": "/data/pending_tool_calls", "value": [] }
]
```

The patch is generated from the already-decoded `ReactState`; it does not diff
the previous JSON blob.

### 3. Journal storage

Schema version 2, no legacy path:

```sql
CREATE TABLE runs (
    run_id TEXT PRIMARY KEY,
    state_json TEXT NOT NULL,          -- header, engine data is null
    state_version INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE run_engine_snapshots (
    run_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    engine_id TEXT NOT NULL,
    schema_revision INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (run_id, version)
);

CREATE TABLE run_engine_patches (
    run_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    base_version INTEGER NOT NULL,
    patch_json TEXT NOT NULL,
    PRIMARY KEY (run_id, version)
);
```

`runs.state_json` contains the small state header:

```json
{
  "run_id": "...",
  "state_version": 42,
  "phase": "AwaitingModel",
  "budget": { "...": 0 },
  "engine_state": {
    "engine_id": "react",
    "schema_revision": 1,
    "data": null
  }
}
```

The real `data` is reconstructed from the snapshot and patches.

A database with schema version 1 is rejected with an explicit error. There is
no migration or dual-format reader.

### 4. Commit policy

For each transition, inside the same transaction as state, event, effects and
outbox:

```text
engine_delta is None                 -> write a full snapshot
state_version == 1                   -> write a full snapshot
state_version % 32 == 0              -> write a full snapshot
patch size > 64 KiB                  -> write a full snapshot
otherwise                            -> write a patch
```

A snapshot and its patches are committed in the same transaction as the
transition. A crash cannot leave a committed state version without its engine
state representation.

### 5. Load and validate

`load_run`:

```text
1. read the runs header
2. find the newest snapshot <= state_version
3. read patches from snapshot.version + 1 through state_version
4. reject a missing snapshot, a gap, a mismatched base_version,
   or a patch that fails to apply
5. replace the header's empty engine data with the rebuilt data
6. return the full AgentState
```

Recovery does not replay events or re-run the Stepper. The journal remains the
authority for the reconstructed state.

### 6. GC and retention

`autractl gc` removes snapshot and patch rows with the Run. Within a retained
Run, keep only:

- the newest snapshot;
- patches after that snapshot.

Older snapshots and earlier patches can be removed because recovery never
starts before the newest snapshot.

## Consequences

- A normal transition writes only the new history fragment instead of the whole
  history.
- Loading a Run reads one snapshot plus the patches after it.
- Steppers that cannot produce a patch still work through full snapshots.
- A malformed patch fails the load instead of silently producing empty history.
- The snapshot interval and patch-size threshold need benchmarks; 32 and 64 KiB
  are initial values, not measured optima.
- The journal schema has no backward-compatibility burden because no deployed
  data needs migration.

## Alternatives rejected

- **Event replay:** requires deterministic Steppers and re-runs decisions on
  recovery.
- **Chunked JSON blobs:** still serializes the full history every step.
- **ReAct-specific history table:** couples the journal to one Stepper.
- **Legacy full-state fallback:** unnecessary without deployed data, and it adds
  a second read/write path to test forever.

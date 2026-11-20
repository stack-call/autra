# Durable Commit Is a Separate Contract

**Status:** accepted

`EffectJournal` had a default `commit_transition` that wrote state and effects
and then enqueued each effect in separate calls. A third-party journal could
inherit that default and still look durable, even though a crash between the
calls could leave an accepted state without its outbox rows. `enqueue_effect`
also defaulted to `Ok(())`.

Durable commit is now a separate contract:

```rust
pub trait DurableTransitionStore: EffectJournal {
    fn journal(&self) -> &dyn EffectJournal;
    fn commit_transition(...) -> Result<(), JournalError>;
}
```

`EffectJournal` keeps the inspection and dispatch surface and has no default
commit. `LocalEngine` holds `Arc<dyn DurableTransitionStore>`, so configuring a
journal means promising the durable commit contract. `SqliteEffectJournal`
implements it with one transaction; `MemoryJournal` implements the same checks
in process for tests and is documented as not crash-safe. `LocalHost`, a
concrete implementation, moved from execution to runtime.

Consequences: a host cannot silently claim durable semantics through a
default; the compiler requires an explicit commit implementation. The
in-process fallback still exists in `MemoryJournal`, but it is an explicit
choice. `EffectJournal` remains usable for inspection and dispatch without
promising recovery.

Alternatives considered: keeping a default commit with a capability flag
(callers can still forget to check it), making `commit_transition` a required
method on `EffectJournal` (simpler, but conflates best-effort and durable
journals), and moving all journal methods into the durable trait (dispatch and
inspection do not need durability).

# Native Durable and Restate Hosts Stay Independent

**Status:** accepted

Restate owns the journal of its workflow invocations. If Autra also kept a
SQLite journal that independently decided execution order, replay would have
two sources of truth.

We decided `NativeDurable` (SQLite) and `RestateHosted` are separate execution
hosts. They share Stepper semantics but not recovery, timer or delivery
guarantees. A Restate adapter maps Run identity to workflow identity and uses
Restate durable operations; it does not write a second SQLite journal.

Consequences: a capability matrix and per-host tests are mandatory; switching
hosts changes guarantees. There is no promise of lossless host migration.

Revisit only if a shared journal abstraction can be proven without duplicating
authority.

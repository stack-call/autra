# Session and Turn Are Domain Concepts, Not Core Behavior

**Status:** accepted

Session and Turn are part of the product's semantics, but a Core state machine
that owns their lifecycle would grow for every Session UI, store or transport
change. Deleting them would lose the distinction between a conversation, an
interaction boundary and a logical task.

We decided `autra-core` keeps `SessionId`, `TurnId` and optional references on
the Run state. The complete Session model, its history and its management live
in `autra-runtime`; Context construction lives in `autra-harness`. There is no
`autra-session` crate yet.

Consequences: a direct `Agent::run` has no Session or Turn id; a Session
generates a fresh `TurnId` for each turn and passes both ids into the Run. A
future SessionStore is separate from the Run/Effect journal, even if both use
the same SQLite database; the association update needs its own transaction
boundary. A `SessionManager`, `TurnRecord` and `SessionStore` are future work.

Alternatives considered: full Session behavior in Core (bloat), removing
Session/Turn (loses the interaction model), and a separate `autra-session` crate
now (premature for one local implementation). Revisit when a second Session
implementation or a durable session store exists.

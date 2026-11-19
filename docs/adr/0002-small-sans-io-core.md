# Small Sans-I/O Core

**Status:** accepted

If the Stepper performs I/O, it becomes hard to test, replay and reason about.
Mixing model calls, tools and state transitions also lets framework
infrastructure leak into the smallest layer.

We decided `autra-core` defines IDs, state, accepted events, Effect intents,
scope and the Stepper trait, and performs no I/O. Effect execution belongs to
an execution host.

Consequences: a coordinator is required; state is serializable; implementations
must watch history copy cost. Core cannot depend on Tokio, a database, a
sandbox or an HTTP client.

Alternatives considered: making runs fully owned by the model provider (Rig
style), and an actor-oriented core. Both make deterministic testing and host
portability harder.

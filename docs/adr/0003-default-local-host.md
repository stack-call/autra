# Default Local Host Without a Database

**Status:** accepted

Developers should not need SQLite, Redis, a server or Kubernetes to try an
Agent. Requiring a durable store also makes the first experience slower and
harder to debug.

We decided `LocalEphemeral` is the default execution host. It uses in-process
state and Tokio, and does not promise recovery after process exit. Durable
hosts are opt-in.

Consequences: a crash loses the Run and its journal. The public API must expose
capability differences rather than silently downgrade. A developer who needs
recovery chooses a different host.

Revisit if recovery is demanded as a default, or if the local host grows
implicit durability.

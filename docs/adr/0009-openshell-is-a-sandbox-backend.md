# OpenShell Is a Sandbox Backend

**Status:** accepted

Tool declarations and Rust traits do not provide OS isolation. An agent that
executes model-generated commands needs a real enforcement boundary.

We decided OpenShell is an optional Sandbox backend for Tool execution (and
later whole-Agent execution). Autra Policy decides what may be requested;
OpenShell enforces filesystem, process, network and resource limits.

Consequences: the adapter must declare capabilities and return typed errors for
unsupported ones. Releasing a sandbox is not a recovery snapshot. OpenShell does
not journal Agent execution.

Revisit if another sandbox backend offers better isolation or portability; the
boundary stays the same.

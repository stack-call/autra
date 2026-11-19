# Product is a native Agent Framework

**Status:** accepted

Autra began with a platform-first direction. That made Kubernetes, actor
placement and a control plane look like prerequisites, which conflicts with
the goal of running one Agent locally.

We decided the product is a Rust-native Agent Framework. Runtime, durability,
sandboxing and compute are optional layers. The framework must be useful with
no database or server.

Consequences: no infrastructure is a prerequisite for the default path; the
project must resist adding platform features to Core. The platform direction
can return only as separate, optional components.

Revisit if real users require a managed multi-tenant service and cannot build
one on the framework.

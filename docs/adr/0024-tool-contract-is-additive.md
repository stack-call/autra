# Tool Contract Is Additive

**Status:** accepted

Splitting the Tool trait into descriptor, contract and execution would improve
clarity, but replacing it outright would break every built-in tool and test.

We added `ToolDescriptor`, `ToolContract`, `ToolInput`, `ToolOutput` and
`ToolContext` with default adapters. Existing tools keep `spec`, `effect_class`,
`requires_approval` and `call`; the runtime now uses `contract()` for approval
and `execute(input, context)` for execution. A richer tool can override those
without touching Core or Runtime.

Consequences: two method families coexist until a future major version. The
runtime passes run id and call id to tools; tools may use them for logging or
policy. Protocol version negotiation and a shared contract-test suite remain
future work.

Alternatives considered: a breaking trait rewrite (high churn), a separate
`ToolV2` trait (two registries), and no context at all (loses correlation).
Revisit when the next major API version is planned.

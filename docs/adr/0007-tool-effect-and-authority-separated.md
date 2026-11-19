# Tool Effect and Authority Are Separated

**Status:** accepted

A tool call suggested by a model is not an authorization. Treating the model's
request as permission lets prompt injection reach the host.

We decided Tool descriptions, Effect classification and execution authority are
separate. A trusted host checks scope, budget and policy before an Effect is
dispatched. Approval, when added, binds to the concrete effect and parameters.

Consequences: some apparent simplicity is lost; every externally visible tool
needs a contract. The MVP only ships read-only built-in tools, so the approval
path is not implemented yet.

Revisit when adding write tools, approvals or credential-bearing tools.

# Tool Effect and Authority Are Separated

**Status:** accepted

A tool call suggested by a model is not an authorization. Treating the model's
request as permission lets prompt injection reach the host.

We decided Tool descriptions, Effect classification and execution authority are
separate. A trusted host checks scope, budget and policy before an Effect is
dispatched. Approval binds to the concrete effect and parameters.

Consequences: some apparent simplicity is lost; every externally visible tool
needs a contract. Approval is implemented for `write_file`; durable waits,
expiry, actor identity and a policy registry remain future work.

Revisit when adding more write tools, a policy registry or credential-bearing
tools.

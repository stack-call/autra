# Transition Is a Proposal

**Status:** accepted

The Stepper returns a next state, but the coordinator has not yet accepted it.
Calling the value `Transition` invited readers to treat it as committed.

We renamed the canonical type to `TransitionProposal` and kept `Transition` as
a compatibility alias. The local coordinator accepts a proposal in one
`accept_local` boundary; a durable host must atomically commit the state and
effect intents there instead.

Consequences: the API names the proposal/commit distinction. Existing code
using `Transition` keeps compiling, but new code should use
`TransitionProposal`.

Alternatives considered: a breaking rename (needless churn), no rename
(ambiguous ownership), and a separate commit type (deferred until the durable
host). Revisit when a host-specific commit API lands.

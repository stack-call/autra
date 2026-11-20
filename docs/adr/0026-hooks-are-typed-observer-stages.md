# Hooks Are Typed Observer Stages

**Status:** accepted

A global `on_before_everything` / `on_after_everything` hook system makes
execution order hard to reason about and tempts extensions to mutate accepted
facts.

We added an `ObserverHook` with four typed stages: run started, model request,
effect accepted and run finished. Observers only see the lifecycle; they cannot
change execution. Behaviour modifiers and Policy Authority remain future,
host-installed stages.

Consequences: observability can be attached explicitly with
`AgentBuilder::observer`. A future behaviour hook must declare its stage and
cannot bypass scope or approval. Policy Authority is not a normal registration.

Alternatives considered: a generic middleware chain (opaque order), event-bus
subscriptions (no typed stages), and no hooks (only tracing spans). Revisit when
a real behaviour-modifier need appears.

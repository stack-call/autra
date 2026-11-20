# Concepts

The canonical vocabulary is in [`../GLOSSARY.md`](../GLOSSARY.md). This page
explains how the objects relate.

## Identity

```text
AgentDefinition
   └─ AgentRevision
        ├─ Session s1
        │   ├─ Turn t1 ── Run r1 ── Completed
        │   └─ Turn t2 ── Run r2 ── WaitingApproval
        │                       ├─ ChildRun r2a
        │                       └─ ChildRun r2b
        └─ Session s2
            └─ Turn t3 ── Run r3 ── Running
```

- **Session** is a semantic container for turns with one caller. It is not a
  process, worker or connection.
- **Turn** is one input-to-response boundary.
- **Run** is one logical task. It owns the authoritative state.
- **ChildRun** is a Run created by a parent, constrained by the parent scope.
- **RunAttempt** is one physical execution attempt; it exists only in a host
  that may retry or recover.
- **Effect** is one observable I/O intent and its accepted outcome.

`Session != Run != Task != Worker`. A Tokio task or remote worker is an
execution detail, never a domain identity.

In the current harness, `Session` keeps the accepted `HistoryItem` list in
memory. `run_turn` takes `&mut self`, so turns are sequential; each turn is a
fresh Run with a fresh budget, Run id and Turn id, seeded with the Session
history. The Run state carries the Session and Turn ids as optional references;
a direct `Agent::run` has neither.

## Execution objects

- **Stepper** turns accepted events into the next state plus Effect intents.
  It is Sans-I/O and does not call a model or tool.
- **AcceptedEvent** is ordered and authoritative input to the Stepper.
- **Durable Event** is an accepted event recorded by a durable host.
- **Stream Event** is a non-authoritative real-time notification; it may be
  lost or repeated.
- **Telemetry** is observability data; it cannot replace execution state.

## Context objects

- **Context View** is the versioned model input for one call: system
  instructions, selected history, tool observations, skills and budget.
- **Journal** is the authoritative record owned by the execution host.
- **Workspace** is the file environment a tool acts on. A sandbox enforces
  isolation; a workspace is the environment being isolated.

Compacting a Context View never changes the Journal. See
[`context-and-memory.md`](context-and-memory.md).

# Basic evaluation suite

**Status:** ready-for-agent

**Source:** design spec §17.4 (Agent quality evaluation) and M6.

## Problem Statement

There is no way to run a set of tasks against an Agent and report success. A
demo or a single test does not show whether an Agent still completes tasks after
a change.

## Solution

Add an `autra-eval` crate with `EvalTask`, `EvalResult`, `EvalReport` and
`run_tasks`. Each task runs the Agent once; a task passes when the answer
contains the expected text. Errors are recorded as failed results with the
error message.

## Implementation Decisions

- Sequential execution against one Agent; each task is an independent Run.
- `EvalTask` has a name, input and expected substring.
- `EvalReport` exposes results, passed/failed counts and pass rate.
- No live provider is required; EchoModel and FakeModel make the suite
  deterministic.
- Evaluation measures Agent outcomes, not runtime correctness. Duplicate
  effects, orphan tasks and recovery claims need the fault tests.

## Testing Decisions

- EchoModel tasks produce one pass and one failure; the report counts and rate
  are correct.
- A model failure is recorded with its error message.
- The crate is included in the workspace and the default test suite.

## Out of Scope

Datasets and fixtures from disk, parallel evaluation, LLM-as-judge, cost/latency
metrics and a dashboard.

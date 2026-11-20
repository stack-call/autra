# Evaluation

`autra-eval` runs a list of tasks against one Agent and reports which answers
contain the expected text.

```rust
use autra_eval::{EvalTask, run_tasks};

let tasks = vec![
    EvalTask::new("greeting", "hello", "echo: hello"),
    EvalTask::new("missing", "world", "not present"),
];
let report = run_tasks(&agent, &tasks).await;
assert_eq!(report.passed(), 1);
assert_eq!(report.failed(), 1);
```

`EvalResult` keeps the answer or the error. `EvalReport` exposes `results`,
`passed()`, `failed()` and `pass_rate()`.

## Boundaries

- Evaluation measures **Agent outcomes**: task success, answer content and
  errors. It does not prove runtime correctness.
- Duplicate effects, orphan tasks, cancellation races and recovery are covered
  by runtime fault tests, not by a task suite.
- EchoModel and FakeModel make the default suite deterministic; a real provider
  is optional and must be recorded with its model/version.

## Not implemented

Datasets and fixtures from disk, parallel evaluation, LLM-as-judge, cost and
latency metrics, and a dashboard.

use autra_eval::{EvalTask, run_tasks};
use autra_harness::{EchoModel, FakeModel};
use autra_runtime::Agent;

#[tokio::test]
async fn reports_passes_and_failures() {
    let agent = Agent::builder("eval").model(EchoModel).build().unwrap();
    let tasks = vec![
        EvalTask::new("greeting", "hello", "echo: hello"),
        EvalTask::new("missing", "world", "not present"),
    ];

    let report = run_tasks(&agent, &tasks).await;

    assert_eq!(report.results.len(), 2);
    assert_eq!(report.passed(), 1);
    assert_eq!(report.failed(), 1);
    assert_eq!(report.pass_rate(), 0.5);
    assert!(report.results[0].passed);
    assert_eq!(report.results[0].answer.as_deref(), Some("echo: hello"));
    assert!(!report.results[1].passed);
}

#[tokio::test]
async fn records_run_errors_as_failed_tasks() {
    let agent = Agent::builder("eval-error")
        .model(FakeModel::new(Vec::new()))
        .build()
        .unwrap();
    let tasks = vec![EvalTask::new("broken", "hello", "anything")];

    let report = run_tasks(&agent, &tasks).await;

    assert_eq!(report.failed(), 1);
    assert!(report.results[0].error.is_some());
    assert!(report.results[0].answer.is_none());
}

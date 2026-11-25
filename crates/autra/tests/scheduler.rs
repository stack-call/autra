use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use autra::{Agent, Dispatcher, SchedulerLimits};
use autra_core::{HistoryItem, ModelCallSpec, ModelOutput, RunId, ToolCallRequest, ToolOutcome};
use autra_execution::ModelError;

struct SlowDispatcher {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[async_trait]
impl Dispatcher for SlowDispatcher {
    async fn call_model(
        &self,
        request: &ModelCallSpec,
        _timeout: Option<Duration>,
    ) -> Result<ModelOutput, ModelError> {
        let saw_tool = request
            .messages
            .iter()
            .any(|item| matches!(item, HistoryItem::Tool(_)));
        if saw_tool {
            return Ok(ModelOutput {
                usage: None,
                text: Some("done".to_owned()),
                tool_calls: Vec::new(),
            });
        }
        Ok(ModelOutput {
            usage: None,
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "slow".to_owned(),
                arguments: "{}".to_owned(),
            }],
        })
    }

    async fn call_tool(
        &self,
        _run_id: &RunId,
        call: &ToolCallRequest,
        _timeout: Option<Duration>,
    ) -> ToolOutcome {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        ToolOutcome {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: "ok".to_owned(),
            is_error: false,
            outcome_unknown: false,
        }
    }
}

#[tokio::test]
async fn two_runs_share_the_global_tool_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let dispatcher = SlowDispatcher {
        active: active.clone(),
        max_active: max_active.clone(),
    };
    let agent = Agent::builder("scheduler")
        .dispatcher(dispatcher)
        .scheduler_limits(SchedulerLimits::new(2, 1, 1))
        .build()
        .unwrap();

    let (first, second) = tokio::join!(agent.run("one"), agent.run("two"));

    assert_eq!(first.unwrap().answer(), Some("done"));
    assert_eq!(second.unwrap().answer(), Some("done"));
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn one_hundred_runs_respect_the_global_tool_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let dispatcher = SlowDispatcher {
        active: active.clone(),
        max_active: max_active.clone(),
    };
    let agent = Arc::new(
        Agent::builder("scheduler-load")
            .dispatcher(dispatcher)
            .scheduler_limits(SchedulerLimits::new(100, 4, 4))
            .build()
            .unwrap(),
    );
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..100 {
        let agent = agent.clone();
        tasks.spawn(async move { agent.run(&format!("run-{index}")).await });
    }

    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().unwrap().answer(), Some("done"));
    }

    assert!(
        max_active.load(Ordering::SeqCst) <= 4,
        "tool concurrency exceeded the global limit"
    );
}

#[tokio::test]
async fn a_pooled_execution_run_matches_the_inline_result() {
    use autra::EffectJournal;
    use autra_core::{ModelOutput, ToolCallRequest};
    use autra_durable_sqlite::SqliteEffectJournal;
    use autra_harness::{EchoTool, FakeModel};

    fn model() -> FakeModel {
        FakeModel::new(vec![
            ModelOutput {
                usage: None,
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                    arguments: r#"{"text":"hi"}"#.to_owned(),
                }],
            },
            ModelOutput {
                usage: None,
                text: Some("done".to_owned()),
                tool_calls: Vec::new(),
            },
        ])
    }

    let dir = tempfile::tempdir().unwrap();
    let inline_path = dir.path().join("inline.db");
    let inline = Agent::builder("pooled")
        .model(model())
        .tool(EchoTool)
        .journal(SqliteEffectJournal::open(&inline_path).unwrap())
        .build()
        .unwrap();
    let inline_outcome = inline.run("go").await.unwrap();

    let pooled_path = dir.path().join("pooled.db");
    let pooled = Agent::builder("pooled")
        .model(model())
        .tool(EchoTool)
        .journal(SqliteEffectJournal::open(&pooled_path).unwrap())
        .pooled_execution(true)
        .build()
        .unwrap();
    let pooled_outcome = pooled.run("go").await.unwrap();

    assert_eq!(pooled_outcome.answer(), inline_outcome.answer());
    assert_eq!(pooled_outcome.status(), inline_outcome.status());
    assert_eq!(
        pooled_outcome.effects().len(),
        inline_outcome.effects().len()
    );
    assert_eq!(
        pooled_outcome
            .effects()
            .iter()
            .map(|e| e.status)
            .collect::<Vec<_>>(),
        inline_outcome
            .effects()
            .iter()
            .map(|e| e.status)
            .collect::<Vec<_>>()
    );
    let journal = SqliteEffectJournal::open(&pooled_path).unwrap();
    assert_eq!(
        journal
            .load_run(&pooled_outcome.state().run_id)
            .unwrap()
            .unwrap(),
        *pooled_outcome.state()
    );
}

#[tokio::test]
async fn pooled_execution_resolves_approvals_before_dispatch() {
    use async_trait::async_trait;
    use autra::{ApprovalDecision, ApprovalRequest, Approver};
    use autra_core::{ModelOutput, ToolCallRequest};
    use autra_durable_sqlite::SqliteEffectJournal;
    use autra_harness::{FakeModel, LocalWorkspace, WriteFileTool};

    struct Approve;

    #[async_trait]
    impl Approver for Approve {
        async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
            ApprovalDecision::Approved
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let db = dir.path().join("pooled-approval.db");
    let agent = Agent::builder("pooled-approval")
        .model(FakeModel::new(vec![
            ModelOutput {
                usage: None,
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "write_file".to_owned(),
                    arguments: r#"{"path":"note.txt","content":"hello"}"#.to_owned(),
                }],
            },
            ModelOutput {
                usage: None,
                text: Some("done".to_owned()),
                tool_calls: Vec::new(),
            },
        ]))
        .tool(WriteFileTool::new(workspace))
        .approver(Approve)
        .journal(SqliteEffectJournal::open(&db).unwrap())
        .pooled_execution(true)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == autra_core::EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, autra_core::EffectStatus::Committed);
}

#[tokio::test]
async fn a_pooled_run_is_woken_by_cancellation() {
    use std::time::Duration;

    use async_trait::async_trait;
    use autra_core::{CancelFlag, EffectClass, ModelOutput, ToolCallRequest, ToolSpec};
    use autra_durable_sqlite::SqliteEffectJournal;
    use autra_harness::{FakeModel, Tool, ToolError};

    struct SlowTool;

    #[async_trait]
    impl Tool for SlowTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "slow".to_owned(),
                description: "sleeps".to_owned(),
                input_schema: "{}".to_owned(),
            }
        }

        fn effect_class(&self) -> EffectClass {
            EffectClass::ReadOnly
        }

        async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
            tokio::time::sleep(Duration::from_millis(400)).await;
            Ok("done".to_owned())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pooled-cancel.db");
    let agent = Agent::builder("pooled-cancel")
        .model(FakeModel::new(vec![
            ModelOutput {
                usage: None,
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "slow".to_owned(),
                    arguments: "{}".to_owned(),
                }],
            },
            ModelOutput {
                usage: None,
                text: Some("done".to_owned()),
                tool_calls: Vec::new(),
            },
        ]))
        .tool(SlowTool)
        .journal(SqliteEffectJournal::open(&db).unwrap())
        .pooled_execution(true)
        .build()
        .unwrap();
    let cancel = CancelFlag::new();
    let options = autra::RunOptions {
        cancel: cancel.clone(),
        ..Default::default()
    };

    let task = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            cancel.cancel();
        })
    };
    let outcome = agent.run_with_options("go", options, |_| {}).await.unwrap();
    task.await.unwrap();

    assert_eq!(outcome.status(), autra::RunStatus::Cancelled);
}

#[tokio::test]
async fn pooled_execution_can_spawn_a_subagent() {
    use autra_core::{ModelOutput, ToolCallRequest};
    use autra_durable_sqlite::SqliteEffectJournal;
    use autra_harness::{EchoModel, FakeModel};

    let child = Agent::builder("child").model(EchoModel).build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pooled-child.db");
    let parent = Agent::builder("parent")
        .model(FakeModel::new(vec![
            ModelOutput {
                usage: None,
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "spawn_subagent".to_owned(),
                    arguments: r#"{"agent":"helper","task":"hi"}"#.to_owned(),
                }],
            },
            ModelOutput {
                usage: None,
                text: Some("done".to_owned()),
                tool_calls: Vec::new(),
            },
        ]))
        .subagent("helper", "echoes", child)
        .journal(SqliteEffectJournal::open(&db).unwrap())
        .pooled_execution(true)
        .build()
        .unwrap();

    let outcome = parent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == autra_core::EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, autra_core::EffectStatus::Committed);
    assert!(tool.result.as_deref().unwrap_or("").contains("echo: hi"));
}

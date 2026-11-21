use autra::{Agent, ChildRunSet, JoinPolicy, RunStatus};
use autra_core::{CancelFlag, RunId};
use autra_harness::EchoModel;

#[tokio::test]
async fn a_child_has_its_own_run_id_and_a_parent_link() {
    let agent = Agent::builder("child").model(EchoModel).build().unwrap();
    let parent = RunId::new("parent-run");

    let handle = agent.spawn_child(parent.clone(), CancelFlag::new(), "hello");
    let child_id = handle.run_id().clone();
    let outcome = handle.join().await.unwrap();

    assert_eq!(outcome.state().parent_run_id.as_ref(), Some(&parent));
    assert_ne!(outcome.state().run_id, parent);
    assert_eq!(&child_id, &outcome.state().run_id);
    assert_eq!(outcome.answer(), Some("echo: hello"));
}

#[tokio::test]
async fn parent_cancellation_cancels_the_child() {
    let agent = Agent::builder("child").model(EchoModel).build().unwrap();
    let parent_cancel = CancelFlag::new();

    let handle = agent.spawn_child(RunId::new("parent-run"), parent_cancel.clone(), "hello");
    parent_cancel.cancel();
    let outcome = handle.join().await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
}

#[tokio::test]
async fn cancelling_a_child_does_not_cancel_the_parent() {
    let agent = Agent::builder("child").model(EchoModel).build().unwrap();
    let parent_cancel = CancelFlag::new();

    let handle = agent.spawn_child(RunId::new("parent-run"), parent_cancel.clone(), "hello");
    handle.cancel();
    let outcome = handle.join().await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
    assert!(!parent_cancel.is_cancelled());
}

#[tokio::test]
async fn wait_all_returns_every_child_result() {
    let first = Agent::builder("first").model(EchoModel).build().unwrap();
    let second = Agent::builder("second").model(EchoModel).build().unwrap();
    let parent = RunId::new("parent-run");
    let handles = vec![
        first.spawn_child(parent.clone(), CancelFlag::new(), "one"),
        second.spawn_child(parent, CancelFlag::new(), "two"),
    ];

    let results = ChildRunSet::new(handles, JoinPolicy::WaitAll).join().await;

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result.is_ok()));
    let answers: Vec<Option<&str>> = results
        .iter()
        .map(|result| result.as_ref().unwrap().answer())
        .collect();
    assert_eq!(answers, vec![Some("echo: one"), Some("echo: two")]);
}

#[tokio::test]
async fn fail_fast_stops_after_the_first_error() {
    let failing = Agent::builder("failing")
        .model(autra_harness::FakeModel::new(Vec::new()))
        .build()
        .unwrap();
    let other = Agent::builder("other").model(EchoModel).build().unwrap();
    let parent = RunId::new("parent-run");
    let handles = vec![
        failing.spawn_child(parent.clone(), CancelFlag::new(), "boom"),
        other.spawn_child(parent, CancelFlag::new(), "never joined"),
    ];

    let results = ChildRunSet::new(handles, JoinPolicy::FailFast).join().await;

    assert_eq!(results.len(), 2);
    assert!(results[0].is_err());
    assert!(results[1].is_ok());
}

use std::sync::Arc;

use async_trait::async_trait;
use autra::{
    ApprovalDecision, ApprovalRequest, Approver, ChildExecutionContext, DurableTransitionStore,
    EffectJournal, MemoryJournal,
};
use autra_core::BudgetSnapshot;
use autra_harness::{FakeModel, LocalWorkspace, ModelOutput, ToolCallRequest, WriteFileTool};

struct AutoApprove;

#[async_trait]
impl Approver for AutoApprove {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Approved
    }
}

#[tokio::test]
async fn a_child_can_inherit_a_journal_and_be_resumed() {
    let journal = MemoryJournal::new();
    let child = Agent::builder("child")
        .model(EchoModel)
        .journal(journal.clone())
        .build()
        .unwrap();
    let shared: Arc<dyn DurableTransitionStore> = Arc::new(journal.clone());
    let execution = ChildExecutionContext {
        journal: Some(shared),
        approver: None,
        depth: 0,
    };
    let parent = RunId::new("parent-run");

    let handle = child.spawn_child_with(
        parent.clone(),
        CancelFlag::new(),
        "hi",
        BudgetSnapshot::new(4, 4),
        execution,
        false,
    );
    let child_id = handle.run_id().clone();
    let outcome = handle.join().await.unwrap();

    assert_eq!(outcome.state().parent_run_id.as_ref(), Some(&parent));
    let stored = journal.load_run(&child_id).unwrap().unwrap();
    assert_eq!(stored.parent_run_id.as_ref(), Some(&parent));
    assert!(!journal.load(&child_id).unwrap().is_empty());

    let resumed = child.resume(&child_id).await.unwrap();
    assert_eq!(resumed.answer(), Some("echo: hi"));
}

#[tokio::test]
async fn forwarded_child_events_are_available() {
    let agent = Agent::builder("child").model(EchoModel).build().unwrap();
    let mut handle = agent.spawn_child_with(
        RunId::new("parent-run"),
        CancelFlag::new(),
        "hi",
        BudgetSnapshot::new(4, 4),
        ChildExecutionContext::default(),
        true,
    );
    let mut events = handle.take_events().expect("event stream");
    let collector = tokio::spawn(async move {
        let mut collected = Vec::new();
        while let Some(event) = events.recv().await {
            collected.push(event);
        }
        collected
    });

    handle.join().await.unwrap();
    let events = collector.await.unwrap();

    assert!(
        events
            .iter()
            .any(|event| matches!(event, autra::RunEvent::RunStarted { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, autra::RunEvent::RunCompleted { .. }))
    );
}

#[tokio::test]
async fn a_child_can_inherit_an_approver() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let child = Agent::builder("child")
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
        .build()
        .unwrap();
    let execution = ChildExecutionContext {
        journal: None,
        approver: Some(Arc::new(AutoApprove)),
        depth: 0,
    };

    let outcome = child
        .run_as_child_with(
            RunId::new("parent-run"),
            CancelFlag::new(),
            "go",
            BudgetSnapshot::new(4, 4),
            execution,
            false,
        )
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
}

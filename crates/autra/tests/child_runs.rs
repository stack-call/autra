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

    assert_eq!(results.len(), 1);
    assert!(results[0].is_err());
}

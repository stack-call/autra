use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra::{Agent, RunOptions, RunStatus, Session};
use autra_core::{HistoryItem, ModelCallSpec, ModelOutput};
use autra_harness::{EchoModel, FakeModel, ModelError, ModelProvider};

struct RecordingModel {
    responses: Mutex<VecDeque<ModelOutput>>,
    requests: Arc<Mutex<Vec<ModelCallSpec>>>,
}

#[async_trait]
impl ModelProvider for RecordingModel {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.requests
            .lock()
            .expect("request log poisoned")
            .push(request.clone());
        self.responses
            .lock()
            .expect("response queue poisoned")
            .pop_front()
            .ok_or_else(|| ModelError::new("recording model exhausted"))
    }
}

fn final_answer(text: &str) -> ModelOutput {
    ModelOutput {
        text: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

#[tokio::test]
async fn second_turn_sees_the_first_turn_history() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel {
        responses: Mutex::new(VecDeque::from(vec![
            final_answer("one"),
            final_answer("two"),
        ])),
        requests: requests.clone(),
    };
    let agent = Agent::builder("session").model(model).build().unwrap();
    let mut session = Session::new(agent);

    let first = session.run_turn("hello").await.unwrap();
    let second = session.run_turn("again").await.unwrap();

    assert_eq!(first.answer(), Some("one"));
    assert_eq!(second.answer(), Some("two"));
    assert_eq!(
        first.state().session_id.as_ref(),
        Some(session.session_id())
    );
    assert_eq!(
        second.state().session_id.as_ref(),
        Some(session.session_id())
    );
    assert!(first.state().turn_id.is_some());
    assert!(second.state().turn_id.is_some());
    assert_ne!(first.state().turn_id, second.state().turn_id);
    assert_eq!(session.last_turn_id(), second.state().turn_id.as_ref());
    assert_eq!(session.turn_index(), 2);
    assert_eq!(session.history().len(), 4);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages.len(), 1);
    assert_eq!(requests[1].messages.len(), 3);
    assert!(matches!(requests[1].messages[0], HistoryItem::User(_)));
    assert!(matches!(
        requests[1].messages[1],
        HistoryItem::Assistant { .. }
    ));
    assert!(matches!(requests[1].messages[2], HistoryItem::User(_)));
}

#[test]
fn sessions_have_distinct_identities() {
    let first = Session::new(Agent::builder("first").model(EchoModel).build().unwrap());
    let second = Session::new(Agent::builder("second").model(EchoModel).build().unwrap());

    assert_ne!(first.session_id(), second.session_id());
    assert_eq!(first.turn_index(), 0);
    assert!(first.last_turn_id().is_none());
}

#[tokio::test]
async fn a_cancelled_turn_stores_the_accepted_history() {
    let agent = Agent::builder("cancel")
        .model(FakeModel::new(Vec::new()))
        .build()
        .unwrap();
    let mut session = Session::new(agent);
    let options = RunOptions::default();
    options.cancel.cancel();

    let outcome = session
        .run_turn_with_options("hello", options, |_| {})
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
    assert_eq!(session.history().len(), 1);
    assert!(matches!(session.history()[0], HistoryItem::User(_)));
}

#[tokio::test]
async fn a_failed_turn_leaves_the_history_unchanged() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel {
        responses: Mutex::new(VecDeque::new()),
        requests,
    };
    let agent = Agent::builder("failure").model(model).build().unwrap();
    let mut session = Session::new(agent);

    let error = session.run_turn("hello").await.unwrap_err();

    assert!(error.to_string().contains("recording model exhausted"));
    assert!(session.history().is_empty());
    assert_eq!(session.turn_index(), 1);
}

#[tokio::test]
async fn a_direct_agent_run_has_no_session_or_turn_identity() {
    let agent = Agent::builder("direct").model(EchoModel).build().unwrap();

    let outcome = agent.run("hello").await.unwrap();

    assert!(outcome.state().session_id.is_none());
    assert!(outcome.state().turn_id.is_none());
}

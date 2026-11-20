use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra::{
    Agent, RunStatus, Session, SessionRecord, SessionStore, SessionStoreError, TurnRecord,
    TurnStatus,
};
use autra_core::{ModelCallSpec, ModelOutput, SessionId};
use autra_harness::{EchoModel, ModelError, ModelProvider};

#[derive(Clone, Default)]
struct MemorySessionStore {
    sessions: Arc<Mutex<BTreeMap<SessionId, SessionRecord>>>,
}

impl SessionStore for MemorySessionStore {
    fn create_session(&self, session_id: &SessionId) -> Result<(), SessionStoreError> {
        self.sessions
            .lock()
            .unwrap()
            .entry(session_id.clone())
            .or_insert_with(|| SessionRecord {
                session_id: session_id.clone(),
                turns: Vec::new(),
            });
        Ok(())
    }

    fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        Ok(self.sessions.lock().unwrap().get(session_id).cloned())
    }

    fn save_turn(
        &self,
        session_id: &SessionId,
        turn: &TurnRecord,
    ) -> Result<(), SessionStoreError> {
        let mut sessions = self.sessions.lock().unwrap();
        let record = sessions
            .entry(session_id.clone())
            .or_insert_with(|| SessionRecord {
                session_id: session_id.clone(),
                turns: Vec::new(),
            });
        match record
            .turns
            .iter_mut()
            .find(|existing| existing.index == turn.index)
        {
            Some(existing) => *existing = turn.clone(),
            None => record.turns.push(turn.clone()),
        }
        record.turns.sort_by_key(|turn| turn.index);
        Ok(())
    }
}

struct FailingModel;

#[async_trait]
impl ModelProvider for FailingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Err(ModelError::new("model-boom"))
    }
}

fn echo_agent() -> Agent {
    Agent::builder("session").model(EchoModel).build().unwrap()
}

#[tokio::test]
async fn a_persisted_session_records_turns_and_restores_history() {
    let store = Arc::new(MemorySessionStore::default());
    let session_id = SessionId::new("session-1");
    let mut session = Session::open(echo_agent(), store.clone(), session_id.clone()).unwrap();

    session.run_turn("one").await.unwrap();
    session.run_turn("two").await.unwrap();

    assert_eq!(session.turn_index(), 2);
    assert_eq!(session.history().len(), 4);
    let record = store.load_session(&session_id).unwrap().unwrap();
    assert_eq!(record.turns.len(), 2);
    assert_eq!(record.turns[0].status, TurnStatus::Completed);
    assert_eq!(record.turns[1].answer.as_deref(), Some("echo: two"));

    let mut reopened = Session::open(echo_agent(), store.clone(), session_id.clone()).unwrap();
    assert_eq!(reopened.turn_index(), 2);
    assert_eq!(reopened.history().len(), 4);
    reopened.run_turn("three").await.unwrap();
    assert_eq!(reopened.turn_index(), 3);
    assert_eq!(
        store
            .load_session(&session_id)
            .unwrap()
            .unwrap()
            .turns
            .len(),
        3
    );
}

#[tokio::test]
async fn a_failed_turn_is_recorded() {
    let store = Arc::new(MemorySessionStore::default());
    let session_id = SessionId::new("session-fail");
    let agent = Agent::builder("session")
        .model(FailingModel)
        .build()
        .unwrap();
    let mut session = Session::open(agent, store.clone(), session_id.clone()).unwrap();

    let error = session.run_turn("hello").await.unwrap_err();

    assert!(error.to_string().contains("model-boom"));
    let record = store.load_session(&session_id).unwrap().unwrap();
    assert_eq!(record.turns.len(), 1);
    assert_eq!(record.turns[0].status, TurnStatus::Failed);
    assert!(
        record.turns[0]
            .error
            .as_deref()
            .unwrap()
            .contains("model-boom")
    );
    assert!(record.turns[0].run_id.is_none());
}

#[tokio::test]
async fn a_cancelled_turn_is_recorded_as_cancelled() {
    use autra::RunOptions;
    use autra_core::CancelFlag;

    let store = Arc::new(MemorySessionStore::default());
    let session_id = SessionId::new("session-cancel");
    let mut session = Session::open(echo_agent(), store.clone(), session_id.clone()).unwrap();
    let cancel = CancelFlag::new();
    cancel.cancel();

    let outcome = session
        .run_turn_with_options(
            "hello",
            RunOptions {
                cancel,
                ..RunOptions::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
    let record = store.load_session(&session_id).unwrap().unwrap();
    assert_eq!(record.turns[0].status, TurnStatus::Cancelled);
}

#[tokio::test]
async fn an_in_memory_session_does_not_require_a_store() {
    let mut session = Session::new(echo_agent());

    session.run_turn("hello").await.unwrap();

    assert_eq!(session.turn_index(), 1);
    assert_eq!(session.history().len(), 2);
}

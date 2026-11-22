use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use autra::{
    Agent, ApprovalRecord, ApprovalStore, ApprovalStoreError, DurableTransitionStore,
    EffectJournal, JournalError, MemoryJournal,
};
use autra_core::{
    AcceptedEvent, AgentState, EffectId, EffectRecord, EventKind, ModelCallSpec, ModelOutput, RunId,
};
use autra_harness::{EchoModel, ModelError, ModelProvider};

#[tokio::test]
async fn a_run_persists_its_effect_records() {
    let journal = MemoryJournal::new();
    let agent = Agent::builder("journal")
        .model(EchoModel)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.run("hello").await.unwrap();

    let stored = journal.load(&outcome.state().run_id).unwrap();
    assert_eq!(
        journal.load_run(&outcome.state().run_id).unwrap().as_ref(),
        Some(outcome.state())
    );
    assert_eq!(stored, outcome.effects());
    assert_eq!(stored.len(), 1);
    let events = journal.load_events(&outcome.state().run_id).unwrap();
    assert!(!events.is_empty());
    assert!(matches!(events[0].kind, EventKind::RunStarted { .. }));
}

#[derive(Clone, Default)]
struct FailingJournal {
    commits: Arc<AtomicUsize>,
    saves: Arc<AtomicUsize>,
}

impl EffectJournal for FailingJournal {
    fn record(&self, _run_id: &RunId, _effect: &EffectRecord) -> Result<(), JournalError> {
        Ok(())
    }

    fn load(&self, _run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError> {
        Ok(Vec::new())
    }

    fn save_run(&self, _state: &AgentState) -> Result<(), JournalError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl ApprovalStore for FailingJournal {
    fn save_approval(&self, _record: &ApprovalRecord) -> Result<(), ApprovalStoreError> {
        Ok(())
    }

    fn load_approval(
        &self,
        _effect_id: &EffectId,
    ) -> Result<Option<ApprovalRecord>, ApprovalStoreError> {
        Ok(None)
    }
}

impl DurableTransitionStore for FailingJournal {
    fn journal(&self) -> &dyn EffectJournal {
        self
    }

    fn approval_store(&self) -> &dyn ApprovalStore {
        self
    }

    fn commit_transition(
        &self,
        _run_id: &RunId,
        _expected_version: u64,
        _event: &AcceptedEvent,
        _state: &AgentState,
        _new_effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        Err(JournalError::new("commit-boom"))
    }
}

struct CountingModel(Arc<AtomicUsize>);

#[async_trait]
impl ModelProvider for CountingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        })
    }
}

#[tokio::test]
async fn a_failed_commit_does_not_apply_state_or_dispatch() {
    let journal = FailingJournal::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::builder("commit-fail")
        .model(CountingModel(calls.clone()))
        .journal(journal.clone())
        .build()
        .unwrap();

    let error = agent.run("go").await.unwrap_err();

    assert!(error.to_string().contains("commit-boom"));
    assert_eq!(journal.commits.load(Ordering::SeqCst), 1);
    assert_eq!(
        journal.saves.load(Ordering::SeqCst),
        0,
        "the uncommitted state must not be persisted separately"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no Effect may be dispatched after a failed commit"
    );
}

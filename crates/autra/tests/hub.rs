use std::sync::Arc;

use autra::{Agent, RunHub};
use autra_core::AgentPhase;
use autra_harness::EchoModel;

#[tokio::test]
async fn a_run_publishes_a_final_snapshot() {
    let hub = Arc::new(RunHub::new());
    let agent = Agent::builder("hub")
        .model(EchoModel)
        .run_hub(hub.clone())
        .build()
        .unwrap();

    let outcome = agent.run("hi").await.unwrap();
    let run_id = outcome.state().run_id.clone();

    let snapshot = hub.snapshot(&run_id).unwrap();
    assert_eq!(snapshot.phase, AgentPhase::Completed);
    assert_eq!(snapshot.final_answer.as_deref(), Some("echo: hi"));
    assert!(snapshot.last_event_seq > 0);
    assert_eq!(snapshot.effect_count, 1);
    assert_eq!(hub.runs().len(), 1);
    let subscription = hub.subscribe(&run_id).unwrap();
    assert_eq!(subscription.snapshot, snapshot);
}

use async_trait::async_trait;
use autra::{EffectJournal, MemoryJournal};
use autra_core::{AgentState, BudgetSnapshot, ModelCallSpec, RunId};
use autra_harness::{ModelError, ModelOutput, ModelProvider};

struct FailingModel;

#[async_trait]
impl ModelProvider for FailingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Err(ModelError::new("model-boom"))
    }
}

#[tokio::test]
async fn resuming_a_completed_run_publishes_a_final_snapshot() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-hub-resume");
    let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(4, 4));
    state.phase = AgentPhase::Completed;
    state.final_answer = Some("done".to_owned());
    journal.save_run(&state).unwrap();
    let hub = Arc::new(RunHub::new());
    let agent = Agent::builder("hub")
        .model(EchoModel)
        .journal(journal)
        .run_hub(hub.clone())
        .build()
        .unwrap();

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let snapshot = hub.snapshot(&run_id).unwrap();
    assert_eq!(snapshot.phase, AgentPhase::Completed);
    assert_eq!(snapshot.final_answer.as_deref(), Some("done"));
}

#[tokio::test]
async fn a_failed_model_run_publishes_a_failed_snapshot() {
    let hub = Arc::new(RunHub::new());
    let agent = Agent::builder("hub")
        .model(FailingModel)
        .run_hub(hub.clone())
        .build()
        .unwrap();

    assert!(agent.run("x").await.is_err());
    let runs = hub.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].phase, AgentPhase::Failed);
}

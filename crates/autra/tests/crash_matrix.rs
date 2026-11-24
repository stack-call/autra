//! Crash-boundary checks: a persisted outcome is not executed twice, and a
//! dispatched non-idempotent effect becomes unknown instead of being retried.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use autra::{Agent, EffectJournal, MemoryJournal};
use autra_core::{
    AgentPhase, AgentState, BudgetSnapshot, EffectClass, EffectId, EffectIntent, EffectKind,
    EffectRecord, EffectStatus, HistoryItem, ModelCallSpec, ModelOutput, RunId, ToolCallRequest,
    ToolCallSpec,
};
use autra_harness::{FakeModel, ModelError, ModelProvider, Tool, ToolError, react_engine_state};

fn call() -> ToolCallRequest {
    ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: "counted".to_owned(),
        arguments: "{}".to_owned(),
    }
}

fn state(run_id: &RunId) -> AgentState {
    let call = call();
    let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(4, 4));
    state.phase = AgentPhase::AwaitingTool;
    state.engine_state = react_engine_state(
        vec![
            HistoryItem::User("go".to_owned()),
            HistoryItem::Assistant {
                text: None,
                tool_calls: vec![call.clone()],
            },
        ],
        vec![call],
    );
    state.budget.turns_used = 1;
    state.budget.tool_calls_used = 1;
    state
}

fn effect(call: &ToolCallRequest, status: EffectStatus) -> EffectRecord {
    let intent = EffectIntent::CallTool(ToolCallSpec { call: call.clone() });
    let mut record = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ToolCall,
        Some(call.name.clone()),
        Some(call.call_id.clone()),
        "digest".to_owned(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    record.set_status(status);
    record
}

struct CountedTool(Arc<AtomicUsize>);

#[async_trait]
impl Tool for CountedTool {
    fn spec(&self) -> autra_core::ToolSpec {
        autra_core::ToolSpec {
            name: "counted".to_owned(),
            description: "counts executions".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::NonIdempotent
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("ran".to_owned())
    }
}

fn answer(text: &str) -> ModelOutput {
    ModelOutput {
        usage: None,
        text: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

struct FailingModel;

#[async_trait]
impl ModelProvider for FailingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Err(ModelError::new("model must not be called"))
    }
}

#[tokio::test]
async fn a_dispatched_non_idempotent_effect_is_not_retried_after_a_crash() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("crash-dispatched");
    journal.save_run(&state(&run_id)).unwrap();
    let call = call();
    let mut record = effect(&call, EffectStatus::Dispatched);
    record.record_attempt(
        autra_core::AttemptStatus::OutcomeUnknown,
        None,
        Some("crash while in flight".to_owned()),
    );
    journal.record(&run_id, &record).unwrap();
    journal.enqueue_effect(&run_id, &record).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::builder("crash")
        .model(FakeModel::new(vec![answer("recovered")]))
        .tool(CountedTool(calls.clone()))
        .journal(journal.clone())
        .build()
        .unwrap();

    agent.resume(&run_id).await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_committed_effect_still_in_the_outbox_is_not_executed_again() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("crash-committed");
    journal.save_run(&state(&run_id)).unwrap();
    let call = call();
    let mut record = effect(&call, EffectStatus::Authorized);
    journal.enqueue_effect(&run_id, &record).unwrap();
    record.set_status(EffectStatus::Committed);
    record.result = Some("already ran".to_owned());
    journal.record(&run_id, &record).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::builder("crash")
        .model(FailingModel)
        .tool(CountedTool(calls.clone()))
        .journal(journal.clone())
        .build()
        .unwrap();

    let results = agent.drain_outbox(10).await.unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        journal
            .claim_due_effects(10, "check")
            .unwrap()
            .is_empty()
    );
}

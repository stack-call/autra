use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use autra_core::{
    AgentPhase, AgentState, AttemptStatus, BudgetSnapshot, EffectClass, EffectId, EffectIntent,
    EffectKind, EffectRecord, EffectStatus, HistoryItem, ModelCallSpec, ModelOutput, RunId,
    ToolCallRequest, ToolCallSpec,
};
use autra_harness::{EchoModel, EchoTool, FakeModel, Tool, ToolError};
use autra_runtime::{Agent, EffectJournal, MemoryJournal, RunError, RunStatus};

struct SensitiveTool {
    calls: Arc<AtomicUsize>,
}

impl SensitiveTool {
    fn new(calls: Arc<AtomicUsize>) -> Self {
        Self { calls }
    }
}

#[async_trait]
impl Tool for SensitiveTool {
    fn spec(&self) -> autra_core::ToolSpec {
        autra_core::ToolSpec {
            name: "sensitive".to_owned(),
            description: "A non-idempotent test tool.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::NonIdempotent
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("ran".to_owned())
    }
}

fn call() -> ToolCallRequest {
    ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"text":"hi"}"#.to_owned(),
    }
}

fn base_state(run_id: &RunId, phase: AgentPhase) -> AgentState {
    let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(8, 8));
    state.phase = phase;
    state.history = vec![HistoryItem::User("go".to_owned())];
    state.budget.turns_used = 1;
    state
}

fn tool_effect(call: &ToolCallRequest, status: EffectStatus) -> EffectRecord {
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

fn model_effect(status: EffectStatus) -> EffectRecord {
    let intent = EffectIntent::CallModel(ModelCallSpec {
        system: None,
        messages: vec![HistoryItem::User("go".to_owned())],
        tools: Vec::new(),
    });
    let mut record = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ModelCall,
        None,
        None,
        "digest".to_owned(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    record.set_status(status);
    record
}

#[tokio::test]
async fn resume_dispatches_an_authorized_tool_then_continues() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-resume-tool");
    let call = call();
    let mut state = base_state(&run_id, AgentPhase::AwaitingTool);
    state.history.push(HistoryItem::Assistant {
        text: None,
        tool_calls: vec![call.clone()],
    });
    state.pending_tool_calls = vec![call.clone()];
    state.budget.tool_calls_used = 1;
    journal.save_run(&state).unwrap();
    let effect = tool_effect(&call, EffectStatus::Authorized);
    journal.record(&run_id, &effect).unwrap();
    journal.enqueue_effect(&run_id, &effect).unwrap();

    let agent = Agent::builder("resume")
        .model(FakeModel::new(vec![ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }]))
        .tool(EchoTool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("done"));
    let stored = journal.load(&run_id).unwrap();
    let tool = stored
        .iter()
        .find(|record| record.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Committed);
    assert_eq!(tool.result.as_deref(), Some("hi"));
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn resume_does_not_retry_a_dispatched_tool() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-resume-unknown");
    let call = call();
    let mut state = base_state(&run_id, AgentPhase::AwaitingTool);
    state.pending_tool_calls = vec![call.clone()];
    state.history.push(HistoryItem::Assistant {
        text: None,
        tool_calls: vec![call.clone()],
    });
    journal.save_run(&state).unwrap();
    let effect = tool_effect(&call, EffectStatus::Dispatched);
    journal.record(&run_id, &effect).unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::builder("resume")
        .model(FakeModel::new(vec![ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }]))
        .tool(SensitiveTool::new(calls.clone()))
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let stored = journal.load(&run_id).unwrap();
    let tool = stored
        .iter()
        .find(|record| record.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::OutcomeUnknown);
    assert_eq!(tool.attempts[0].status, AttemptStatus::OutcomeUnknown);
    let tool_outcome = outcome
        .state()
        .history
        .iter()
        .find_map(|item| match item {
            HistoryItem::Tool(outcome) => Some(outcome),
            _ => None,
        })
        .unwrap();
    assert!(tool_outcome.outcome_unknown);
}

#[tokio::test]
async fn resume_retires_an_interrupted_model_call_and_reissues_it() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-resume-model");
    let state = base_state(&run_id, AgentPhase::AwaitingModel);
    journal.save_run(&state).unwrap();
    let stale = model_effect(EffectStatus::Dispatched);
    journal.record(&run_id, &stale).unwrap();
    journal.enqueue_effect(&run_id, &stale).unwrap();

    let agent = Agent::builder("resume")
        .model(FakeModel::new(vec![ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }]))
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("done"));
    let stored = journal.load(&run_id).unwrap();
    let stale = stored.iter().find(|record| record.id == stale.id).unwrap();
    assert_eq!(stale.status, EffectStatus::OutcomeUnknown);
    let model_calls: Vec<&EffectRecord> = stored
        .iter()
        .filter(|record| record.kind == EffectKind::ModelCall)
        .collect();
    assert_eq!(model_calls.len(), 2);
    assert_eq!(model_calls[1].status, EffectStatus::Committed);
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn resuming_a_completed_run_returns_the_stored_outcome() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-resume-done");
    let mut state = base_state(&run_id, AgentPhase::Completed);
    state.final_answer = Some("already done".to_owned());
    journal.save_run(&state).unwrap();
    let agent = Agent::builder("resume")
        .model(EchoModel)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("already done"));
}

#[tokio::test]
async fn resume_requires_a_known_checkpoint() {
    let journal = MemoryJournal::new();
    let agent = Agent::builder("resume")
        .model(EchoModel)
        .journal(journal)
        .build()
        .unwrap();

    let error = agent.resume(&RunId::new("missing")).await.unwrap_err();

    assert!(matches!(error, RunError::Protocol(_)));
}

#[tokio::test]
async fn resume_requires_a_journal() {
    let agent = Agent::builder("resume").model(EchoModel).build().unwrap();

    let error = agent.resume(&RunId::new("anything")).await.unwrap_err();

    assert!(matches!(error, RunError::Protocol(_)));
}

#[tokio::test]
async fn resume_retries_a_read_only_dispatched_tool() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-resume-retry");
    let call = call();
    let mut state = base_state(&run_id, AgentPhase::AwaitingTool);
    state.pending_tool_calls = vec![call.clone()];
    state.history.push(HistoryItem::Assistant {
        text: None,
        tool_calls: vec![call.clone()],
    });
    journal.save_run(&state).unwrap();
    let effect = tool_effect(&call, EffectStatus::Dispatched);
    journal.record(&run_id, &effect).unwrap();

    let agent = Agent::builder("resume")
        .model(FakeModel::new(vec![ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }]))
        .tool(EchoTool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    let stored = journal.load(&run_id).unwrap();
    let tool = stored
        .iter()
        .find(|record| record.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Committed);
    assert_eq!(tool.result.as_deref(), Some("hi"));
    assert_eq!(tool.attempts.len(), 2);
    assert_eq!(tool.attempts[0].status, AttemptStatus::OutcomeUnknown);
    assert_eq!(tool.attempts[1].status, AttemptStatus::Succeeded);
}

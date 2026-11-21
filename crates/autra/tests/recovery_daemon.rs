use autra::{Agent, EffectJournal, MemoryJournal};
use autra_core::{
    AgentPhase, AgentState, BudgetSnapshot, EffectId, EffectIntent, EffectKind, EffectRecord,
    EffectStatus, HistoryItem, ModelCallSpec, ModelOutput, RunId, ToolCallRequest, ToolCallSpec,
};
use autra_harness::{EchoTool, FakeModel, react_engine_state};

fn answer(text: &str) -> ModelOutput {
    ModelOutput {
        usage: None,
        text: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

fn base_state(run_id: &RunId, phase: AgentPhase) -> AgentState {
    let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(8, 8));
    state.phase = phase;
    state.engine_state = react_engine_state(vec![HistoryItem::User("go".to_owned())], Vec::new());
    state.budget.turns_used = 1;
    state
}

fn model_effect() -> EffectRecord {
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
    record.set_status(EffectStatus::Authorized);
    record
}

fn tool_effect(call: &ToolCallRequest) -> EffectRecord {
    let intent = EffectIntent::CallTool(ToolCallSpec { call: call.clone() });
    let mut record = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ToolCall,
        Some(call.name.clone()),
        Some(call.call_id.clone()),
        "digest".to_owned(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    record.set_status(EffectStatus::Authorized);
    record
}

#[tokio::test]
async fn recovery_resumes_an_awaiting_model_run() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("recover-model");
    journal
        .save_run(&base_state(&run_id, AgentPhase::AwaitingModel))
        .unwrap();
    let effect = model_effect();
    journal.record(&run_id, &effect).unwrap();
    journal.enqueue_effect(&run_id, &effect).unwrap();
    let agent = Agent::builder("daemon")
        .model(FakeModel::new(vec![answer("recovered")]))
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.recover_once(10).await.unwrap();

    assert_eq!(report.scanned, 1);
    assert_eq!(report.resumed, 1);
    assert_eq!(report.failed, 0);
    let stored = journal.load_run(&run_id).unwrap().unwrap();
    assert_eq!(stored.phase, AgentPhase::Completed);
    assert_eq!(stored.final_answer.as_deref(), Some("recovered"));
}

#[tokio::test]
async fn recovery_resumes_an_awaiting_tool_run() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("recover-tool");
    let call = ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"text":"hi"}"#.to_owned(),
    };
    let mut state = base_state(&run_id, AgentPhase::AwaitingTool);
    state.engine_state = react_engine_state(
        vec![
            HistoryItem::User("go".to_owned()),
            HistoryItem::Assistant {
                text: None,
                tool_calls: vec![call.clone()],
            },
        ],
        vec![call.clone()],
    );
    state.budget.tool_calls_used = 1;
    journal.save_run(&state).unwrap();
    let effect = tool_effect(&call);
    journal.record(&run_id, &effect).unwrap();
    journal.enqueue_effect(&run_id, &effect).unwrap();
    let agent = Agent::builder("daemon")
        .model(FakeModel::new(vec![answer("done")]))
        .tool(EchoTool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.recover_once(10).await.unwrap();

    assert_eq!(report.resumed, 1);
    let stored = journal.load_run(&run_id).unwrap().unwrap();
    assert_eq!(stored.phase, AgentPhase::Completed);
    let tool = journal
        .load(&run_id)
        .unwrap()
        .into_iter()
        .find(|record| record.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Committed);
}

#[tokio::test]
async fn recovery_skips_terminal_and_failed_runs() {
    let journal = MemoryJournal::new();
    let completed = RunId::new("recover-completed");
    let mut completed_state = base_state(&completed, AgentPhase::Completed);
    completed_state.final_answer = Some("already done".to_owned());
    journal.save_run(&completed_state).unwrap();
    let failed = RunId::new("recover-failed");
    let mut failed_state = base_state(&failed, AgentPhase::Failed);
    failed_state.last_error = Some("boom".to_owned());
    journal.save_run(&failed_state).unwrap();
    let agent = Agent::builder("daemon")
        .model(FakeModel::new(Vec::new()))
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.recover_once(10).await.unwrap();

    assert_eq!(report.scanned, 2);
    assert_eq!(report.resumed, 0);
    assert_eq!(report.failed, 0);
}

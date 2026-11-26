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
async fn recovery_skips_a_run_with_a_live_owner() {
    use std::time::Duration;

    let journal = MemoryJournal::new();
    let run_id = RunId::new("recover-owned");
    journal
        .save_run(&base_state(&run_id, AgentPhase::AwaitingModel))
        .unwrap();
    let effect = model_effect();
    journal.record(&run_id, &effect).unwrap();
    journal.enqueue_effect(&run_id, &effect).unwrap();
    assert!(
        journal
            .claim_run(&run_id, "other-executor", Duration::from_secs(60))
            .unwrap()
    );
    let agent = Agent::builder("daemon")
        .model(FakeModel::new(Vec::new()))
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.recover_once(10).await.unwrap();

    assert_eq!(report.scanned, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.resumed, 0);
    let stored = journal.load_run(&run_id).unwrap().unwrap();
    assert_eq!(stored.phase, AgentPhase::AwaitingModel);
}

#[tokio::test]
async fn resume_refuses_a_run_owned_by_another_executor() {
    use std::time::Duration;

    let journal = MemoryJournal::new();
    let run_id = RunId::new("resume-owned");
    journal
        .save_run(&base_state(&run_id, AgentPhase::AwaitingModel))
        .unwrap();
    assert!(
        journal
            .claim_run(&run_id, "other-executor", Duration::from_secs(60))
            .unwrap()
    );
    let agent = Agent::builder("owner")
        .model(FakeModel::new(vec![answer("should not run")]))
        .journal(journal.clone())
        .build()
        .unwrap();

    let error = agent.resume(&run_id).await.unwrap_err();

    assert!(error.to_string().contains("owned by another executor"));
}

#[tokio::test]
async fn resume_refuses_a_different_agent_profile() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("resume-profile");
    let mut state = base_state(&run_id, AgentPhase::AwaitingModel);
    state.agent_profile = Some("sha256:other".to_owned());
    journal.save_run(&state).unwrap();
    let agent = Agent::builder("owner")
        .model(FakeModel::new(vec![answer("should not run")]))
        .journal(journal.clone())
        .build()
        .unwrap();

    let error = agent.resume(&run_id).await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("created by a different agent profile")
    );
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

#[tokio::test]
async fn the_recovery_supervisor_holds_a_service_lease() {
    let journal = MemoryJournal::new();
    let agent = Agent::builder("daemon")
        .model(FakeModel::new(Vec::new()))
        .journal(journal)
        .build()
        .unwrap();
    let first = agent.recovery_supervisor();
    let lease = first.claim().unwrap();

    let second = agent.recovery_supervisor();
    let error = match second.claim() {
        Ok(_) => panic!("a second supervisor must not claim the lease"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("another recovery supervisor"));

    drop(lease);
    assert!(second.claim().is_ok());
}

#[tokio::test]
async fn recovery_honours_a_persisted_cancel_request() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("recover-cancelled");
    journal
        .save_run(&base_state(&run_id, AgentPhase::AwaitingModel))
        .unwrap();
    let effect = model_effect();
    journal.record(&run_id, &effect).unwrap();
    journal.enqueue_effect(&run_id, &effect).unwrap();
    journal.request_cancel(&run_id, "user asked").unwrap();
    let agent = Agent::builder("daemon")
        .model(FakeModel::new(Vec::new()))
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.recover_once(10).await.unwrap();

    assert_eq!(report.resumed, 1);
    let stored = journal.load_run(&run_id).unwrap().unwrap();
    assert_eq!(stored.phase, AgentPhase::Cancelled);
}

#[tokio::test]
async fn sqlite_recovery_reaches_runs_after_multiple_pages_of_terminal_runs() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("recovery.db");
    let journal = autra_durable_sqlite::SqliteEffectJournal::open(&path).unwrap();
    for index in 0..129 {
        let run_id = RunId::new(format!("completed-{index:03}"));
        journal
            .save_run(&base_state(&run_id, AgentPhase::Completed))
            .unwrap();
    }
    let run_id = RunId::new("recover-after-terminal-page");
    journal
        .save_run(&base_state(&run_id, AgentPhase::AwaitingModel))
        .unwrap();
    let effect = model_effect();
    journal.record(&run_id, &effect).unwrap();
    journal.enqueue_effect(&run_id, &effect).unwrap();
    let agent = Agent::builder("daemon")
        .model(FakeModel::new(vec![answer("recovered after pagination")]))
        .journal(autra_durable_sqlite::SqliteEffectJournal::open(&path).unwrap())
        .build()
        .unwrap();

    let report = agent.recover_once(1).await.unwrap();

    assert_eq!(report.scanned, 130);
    assert_eq!(report.resumed, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(
        journal.load_run(&run_id).unwrap().unwrap().phase,
        AgentPhase::Completed
    );
}

#[tokio::test]
async fn recovery_starts_later_pages_while_a_run_is_waiting_for_the_model() {
    use std::sync::Arc;
    use std::time::Duration;

    use autra_harness::{ModelError, ModelProvider};
    use tokio::sync::Notify;

    struct GatedModel {
        later_started: Arc<Notify>,
        unblock_first: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for GatedModel {
        async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
            if request
                .messages
                .iter()
                .any(|item| matches!(item, HistoryItem::User(text) if text == "first"))
            {
                self.unblock_first.notified().await;
            } else {
                self.later_started.notify_one();
            }
            Ok(answer("recovered"))
        }
    }

    let journal = MemoryJournal::new();
    let first = RunId::new("a-first");
    let later = RunId::new("z-later");
    for (id, input) in [(&first, "first"), (&later, "later")] {
        let mut state = base_state(id, AgentPhase::AwaitingModel);
        state.engine_state =
            react_engine_state(vec![HistoryItem::User(input.to_owned())], Vec::new());
        journal.save_run(&state).unwrap();
    }
    for index in 0..128 {
        let run = RunId::new(format!("terminal-{index:03}"));
        journal
            .save_run(&base_state(&run, AgentPhase::Completed))
            .unwrap();
    }
    let later_started = Arc::new(Notify::new());
    let unblock_first = Arc::new(Notify::new());
    let agent = Agent::builder("daemon")
        .model(GatedModel {
            later_started: later_started.clone(),
            unblock_first: unblock_first.clone(),
        })
        .journal(journal.clone())
        .build()
        .unwrap();

    let pass = agent.recover_once(2);
    let release = async {
        later_started.notified().await;
        unblock_first.notify_one();
    };
    let (report, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(pass, release)
    })
    .await
    .unwrap();
    assert_eq!(report.unwrap().resumed, 2);
    for id in [&first, &later] {
        assert_eq!(
            journal.load_run(id).unwrap().unwrap().phase,
            AgentPhase::Completed
        );
    }
}

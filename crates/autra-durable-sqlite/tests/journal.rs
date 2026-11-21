use autra::{Agent, EffectJournal};
use autra_core::{EffectId, EffectKind, EffectRecord, EventKind, RunId};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_harness::EchoModel;
use tempfile::tempdir;

#[test]
fn effect_records_survive_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("effects.db");
    let run_id = RunId::new("run-durable");
    let record = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ModelCall,
        None,
        None,
        "digest".to_owned(),
    );

    {
        let journal = SqliteEffectJournal::open(&path).unwrap();
        journal.record(&run_id, &record).unwrap();
    }

    let reopened = SqliteEffectJournal::open(&path).unwrap();
    assert_eq!(reopened.load(&run_id).unwrap(), vec![record]);
}

#[tokio::test]
async fn an_agent_run_persists_effects_across_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("effects.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let agent = Agent::builder("durable")
        .model(EchoModel)
        .journal(journal)
        .build()
        .unwrap();

    let outcome = agent.run("hello").await.unwrap();
    drop(agent);

    let reopened = SqliteEffectJournal::open(&path).unwrap();
    assert_eq!(
        reopened.load(&outcome.state().run_id).unwrap(),
        outcome.effects()
    );
    assert_eq!(
        reopened.load_run(&outcome.state().run_id).unwrap().as_ref(),
        Some(outcome.state())
    );
    let events = reopened.load_events(&outcome.state().run_id).unwrap();
    assert!(!events.is_empty());
    assert!(matches!(events[0].kind, EventKind::RunStarted { .. }));
}

#[tokio::test]
async fn a_durable_tool_run_drains_its_outbox() {
    use autra_core::{EffectStatus, ModelOutput, ToolCallRequest};
    use autra_harness::{EchoTool, FakeModel};

    let dir = tempdir().unwrap();
    let path = dir.path().join("tool-run.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let model = FakeModel::new(vec![
        ModelOutput {
            usage: None,
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: r#"{"text":"hi"}"#.to_owned(),
            }],
        },
        ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = Agent::builder("durable")
        .model(model)
        .tool(EchoTool)
        .journal(SqliteEffectJournal::open(&path).unwrap())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();
    let run_id = outcome.state().run_id.clone();
    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.tool_name.as_deref() == Some("echo"))
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Committed);
    assert_eq!(tool.result.as_deref(), Some("hi"));
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
    assert_eq!(
        SqliteEffectJournal::open(&path)
            .unwrap()
            .load(&run_id)
            .unwrap(),
        outcome.effects()
    );
}

#[tokio::test]
async fn a_sqlite_checkpoint_can_be_resumed_after_reopen() {
    use autra_core::{
        AgentPhase, AgentState, BudgetSnapshot, EffectIntent, EffectStatus, HistoryItem,
        ModelOutput, ToolCallRequest, ToolCallSpec,
    };
    use autra_harness::{EchoTool, FakeModel, react_engine_state};

    let dir = tempdir().unwrap();
    let path = dir.path().join("resume.db");
    let run_id = RunId::new("run-resume");
    let call = ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: "echo".to_owned(),
        arguments: r#"{"text":"hi"}"#.to_owned(),
    };
    {
        let journal = SqliteEffectJournal::open(&path).unwrap();
        let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(8, 8));
        state.phase = AgentPhase::AwaitingTool;
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
        state.budget.turns_used = 1;
        state.budget.tool_calls_used = 1;
        journal.save_run(&state).unwrap();

        let intent = EffectIntent::CallTool(ToolCallSpec { call: call.clone() });
        let mut effect = EffectRecord::new(
            EffectId::fresh(),
            EffectKind::ToolCall,
            Some(call.name.clone()),
            Some(call.call_id.clone()),
            "digest".to_owned(),
        )
        .with_intent_json(serde_json::to_string(&intent).unwrap());
        effect.set_status(EffectStatus::Authorized);
        journal.record(&run_id, &effect).unwrap();
        journal.enqueue_effect(&run_id, &effect).unwrap();
    }

    let reopened = SqliteEffectJournal::open(&path).unwrap();
    let agent = Agent::builder("durable-resume")
        .model(FakeModel::new(vec![ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }]))
        .tool(EchoTool)
        .journal(SqliteEffectJournal::open(&path).unwrap())
        .build()
        .unwrap();

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(reopened.load(&run_id).unwrap(), outcome.effects());
    assert!(reopened.claim_due_effects(10, "check").unwrap().is_empty());
}

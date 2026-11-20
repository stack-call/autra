use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, BudgetSnapshot, EffectId, EffectIntent, EffectKind,
    EffectRecord, EffectStatus, EventKind, ModelCallSpec, RunId,
};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_execution::EffectJournal;
use tempfile::tempdir;

fn state(run_id: &RunId, version: u64) -> AgentState {
    let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(4, 4));
    state.phase = AgentPhase::AwaitingModel;
    state.state_version = version;
    state
}

fn event(seq: u64) -> AcceptedEvent {
    AcceptedEvent {
        seq,
        kind: EventKind::RunStarted {
            input: "go".to_owned(),
        },
    }
}

fn authorized_model_effect() -> EffectRecord {
    let intent = EffectIntent::CallModel(ModelCallSpec {
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
    });
    let mut effect = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ModelCall,
        None,
        None,
        "digest".to_owned(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    effect.set_status(EffectStatus::Authorized);
    effect
}

#[test]
fn commit_transition_saves_state_event_effect_and_outbox_together() {
    let dir = tempdir().unwrap();
    let journal = SqliteEffectJournal::open(dir.path().join("commit.db")).unwrap();
    let run_id = RunId::new("run-commit");
    let effect = authorized_model_effect();

    journal
        .commit_transition(
            &run_id,
            0,
            &event(1),
            &state(&run_id, 1),
            std::slice::from_ref(&effect),
        )
        .unwrap();

    let stored = journal.load_run(&run_id).unwrap().unwrap();
    assert_eq!(stored.state_version, 1);
    assert_eq!(journal.load_events(&run_id).unwrap().len(), 1);
    let claimed = journal
        .claim_due_effects_for_run(&run_id, 10, "check")
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].effect_id, effect.id);
}

#[test]
fn a_stale_state_version_is_rejected() {
    let dir = tempdir().unwrap();
    let journal = SqliteEffectJournal::open(dir.path().join("conflict.db")).unwrap();
    let run_id = RunId::new("run-conflict");
    journal
        .commit_transition(&run_id, 0, &event(1), &state(&run_id, 1), &[])
        .unwrap();

    let error = journal
        .commit_transition(&run_id, 0, &event(2), &state(&run_id, 2), &[])
        .unwrap_err();

    assert!(error.to_string().contains("conflict"));
    assert_eq!(journal.load_run(&run_id).unwrap().unwrap().state_version, 1);
    assert_eq!(journal.load_events(&run_id).unwrap().len(), 1);
}

#[test]
fn a_duplicate_event_seq_rolls_the_whole_commit_back() {
    let dir = tempdir().unwrap();
    let journal = SqliteEffectJournal::open(dir.path().join("duplicate.db")).unwrap();
    let run_id = RunId::new("run-duplicate");
    journal
        .commit_transition(&run_id, 0, &event(1), &state(&run_id, 1), &[])
        .unwrap();
    let extra = authorized_model_effect();

    let error = journal
        .commit_transition(&run_id, 1, &event(1), &state(&run_id, 2), &[extra])
        .unwrap_err();

    assert!(error.to_string().contains("cannot record event"));
    assert_eq!(journal.load_run(&run_id).unwrap().unwrap().state_version, 1);
    assert_eq!(journal.load_events(&run_id).unwrap().len(), 1);
    assert!(journal.load(&run_id).unwrap().is_empty());
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

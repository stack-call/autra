use autra_core::{
    AgentPhase, AgentState, BudgetSnapshot, EffectId, EffectKind, EffectRecord, EffectStatus, RunId,
};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_execution::{DurableTransitionStore, EffectJournal};
use tempfile::tempdir;

fn authorized_effect() -> EffectRecord {
    let mut effect = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ToolCall,
        Some("echo".to_owned()),
        Some("call-1".to_owned()),
        "digest".to_owned(),
    )
    .with_intent_json(r#"{"CallTool":{"call":{"call_id":"call-1"}}}"#);
    effect.set_status(EffectStatus::Authorized);
    effect
}

#[test]
fn outbox_survives_reopen_and_supports_claims() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("outbox.db");
    let run_id = RunId::new("run-1");
    let effect = authorized_effect();

    {
        let journal = SqliteEffectJournal::open(&path).unwrap();
        journal.enqueue_effect(&run_id, &effect).unwrap();
    }

    let journal = SqliteEffectJournal::open(&path).unwrap();
    let claimed = journal.claim_due_effects(10, "claim-1").unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].effect_id, effect.id);
    assert_eq!(claimed[0].attempts, 1);

    journal.release_outbox(&effect.id, "claim-1").unwrap();
    assert_eq!(
        journal.claim_due_effects(10, "claim-2").unwrap()[0].attempts,
        2
    );
    journal.complete_outbox(&effect.id, "claim-2").unwrap();
    assert!(journal.claim_due_effects(10, "claim-3").unwrap().is_empty());
}

#[test]
fn a_stale_claim_cannot_record_an_outcome() {
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("fencing.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let run_id = RunId::new("run-fencing");
    let effect = authorized_effect();
    journal.enqueue_effect(&run_id, &effect).unwrap();
    journal
        .claim_due_effects_with_lease(10, "claim-1", Duration::from_secs(1))
        .unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    journal
        .claim_due_effects_with_lease(10, "claim-2", Duration::from_secs(1))
        .unwrap();

    let mut recorded = effect.clone();
    recorded.set_status(EffectStatus::Dispatched);
    let error = journal
        .record_claimed(&run_id, &recorded, "claim-1")
        .unwrap_err();
    assert!(error.is_claim_lost());
    journal
        .record_claimed(&run_id, &recorded, "claim-2")
        .unwrap();
}

#[test]
fn run_leases_are_exclusive_and_can_be_released() {
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("runs.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let run_id = RunId::new("run-lease-owner");

    assert!(
        journal
            .claim_run(&run_id, "owner-1", Duration::from_secs(30))
            .unwrap()
    );
    assert!(
        !journal
            .claim_run(&run_id, "owner-2", Duration::from_secs(30))
            .unwrap()
    );
    assert!(
        journal
            .renew_run(&run_id, "owner-1", Duration::from_secs(60))
            .unwrap()
    );
    assert_eq!(
        journal.run_lease(&run_id).unwrap().unwrap().owner_id,
        "owner-1"
    );

    journal.release_run(&run_id, "owner-1").unwrap();
    assert!(journal.run_lease(&run_id).unwrap().is_none());
    assert!(
        journal
            .claim_run(&run_id, "owner-2", Duration::from_secs(30))
            .unwrap()
    );
}

#[test]
fn a_service_lease_has_one_owner() {
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("service.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();

    assert!(
        journal
            .claim_service("recovery-daemon", "daemon-1", Duration::from_secs(30))
            .unwrap()
    );
    assert!(
        !journal
            .claim_service("recovery-daemon", "daemon-2", Duration::from_secs(30))
            .unwrap()
    );
    assert!(
        journal
            .renew_service("recovery-daemon", "daemon-1", Duration::from_secs(60))
            .unwrap()
    );
    journal
        .release_service("recovery-daemon", "daemon-1")
        .unwrap();
    assert!(
        journal
            .claim_service("recovery-daemon", "daemon-2", Duration::from_secs(30))
            .unwrap()
    );
}

#[test]
fn an_expired_lease_can_be_reclaimed() {
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("lease.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let run_id = RunId::new("run-lease");
    let effect = authorized_effect();
    journal.enqueue_effect(&run_id, &effect).unwrap();

    let first = journal
        .claim_due_effects_with_lease(10, "claim-1", Duration::from_secs(1))
        .unwrap();
    assert_eq!(first.len(), 1);
    let blocked = journal
        .claim_due_effects_with_lease(10, "claim-2", Duration::from_secs(1))
        .unwrap();
    assert!(blocked.is_empty());

    // SQLite datetime has second precision; cross the next second.
    std::thread::sleep(Duration::from_millis(2100));
    let reclaimed = journal
        .claim_due_effects_with_lease(10, "claim-3", Duration::from_secs(1))
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].attempts, 2);

    // The previous owner must not complete a row now owned by claim-3.
    journal.complete_outbox(&effect.id, "claim-1").unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM effect_outbox WHERE effect_id = ?1",
            rusqlite::params![effect.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    journal.complete_outbox(&effect.id, "claim-3").unwrap();
    assert!(
        journal
            .claim_due_effects_with_lease(10, "claim-4", Duration::from_secs(1))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn opening_configures_wal_and_sets_the_schema_version() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.db");
    let _journal = SqliteEffectJournal::open(&path).unwrap();

    let connection = rusqlite::Connection::open(&path).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

#[test]
fn a_newer_schema_version_is_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("future.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", 99_i64)
            .unwrap();
    }

    let error = SqliteEffectJournal::open(&path).err().expect("must fail");

    assert!(error.to_string().contains("newer"));
}

#[test]
fn terminal_commits_use_the_expected_version() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let run_id = RunId::new("terminal");
    let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(4, 4));
    state.phase = AgentPhase::Failed;
    state.last_error = Some("boom".to_owned());
    state.state_version = 1;

    journal.commit_terminal(&run_id, 0, &state).unwrap();

    let stored = journal.load_run(&run_id).unwrap().unwrap();
    assert_eq!(stored.phase, AgentPhase::Failed);
    assert_eq!(stored.state_version, 1);

    let mut stale = state.clone();
    stale.state_version = 2;
    assert!(
        journal.commit_terminal(&run_id, 0, &stale).is_err(),
        "a stale terminal commit must not overwrite newer state"
    );
}

#[test]
fn a_cancel_request_survives_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cancel.db");
    let run_id = RunId::new("run-cancel");

    {
        let journal = SqliteEffectJournal::open(&path).unwrap();
        journal.request_cancel(&run_id, "user asked").unwrap();
    }

    let journal = SqliteEffectJournal::open(&path).unwrap();
    assert_eq!(
        journal.cancel_request(&run_id).unwrap().as_deref(),
        Some("user asked")
    );
    journal.clear_cancel(&run_id).unwrap();
    assert!(journal.cancel_request(&run_id).unwrap().is_none());
}

#[test]
fn a_run_lease_survives_reopen_until_it_expires() {
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("run-lease.db");
    let run_id = RunId::new("run-lease-reopen");

    {
        let journal = SqliteEffectJournal::open(&path).unwrap();
        assert!(
            journal
                .claim_run(&run_id, "owner-1", Duration::from_secs(1))
                .unwrap()
        );
    }

    let journal = SqliteEffectJournal::open(&path).unwrap();
    assert_eq!(
        journal.run_lease(&run_id).unwrap().unwrap().owner_id,
        "owner-1"
    );
    assert!(
        !journal
            .claim_run(&run_id, "owner-2", Duration::from_secs(1))
            .unwrap()
    );

    std::thread::sleep(Duration::from_millis(2100));
    assert!(
        journal
            .claim_run(&run_id, "owner-2", Duration::from_secs(1))
            .unwrap()
    );
}

#[test]
fn gc_removes_only_old_terminal_runs() {
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("gc.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let completed = RunId::new("run-completed");
    let active = RunId::new("run-active");
    let mut completed_state = AgentState::new(completed.clone(), BudgetSnapshot::new(2, 2));
    completed_state.phase = AgentPhase::Completed;
    completed_state.final_answer = Some("done".to_owned());
    let mut active_state = AgentState::new(active.clone(), BudgetSnapshot::new(2, 2));
    active_state.phase = AgentPhase::AwaitingModel;
    journal.save_run(&completed_state).unwrap();
    journal.save_run(&active_state).unwrap();

    std::thread::sleep(Duration::from_millis(1100));
    let dry = journal.gc(Duration::from_secs(0), true).unwrap();
    assert_eq!(dry.runs, 1);
    assert!(journal.load_run(&completed).unwrap().is_some());

    let report = journal.gc(Duration::from_secs(0), false).unwrap();
    assert_eq!(report.runs, 1);
    assert!(journal.load_run(&completed).unwrap().is_none());
    assert!(journal.load_run(&active).unwrap().is_some());
}

#[test]
fn health_reports_live_leases_and_cancel_requests() {
    use std::time::Duration;

    let dir = tempdir().unwrap();
    let path = dir.path().join("health.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let run_id = RunId::new("run-health");
    journal
        .claim_run(&run_id, "owner", Duration::from_secs(30))
        .unwrap();
    journal.request_cancel(&run_id, "stop").unwrap();

    let health = journal.health(autra_execution::now_unix_ms()).unwrap();

    assert_eq!(health.live_run_leases, 1);
    assert_eq!(health.expired_run_leases, 0);
    assert_eq!(health.cancel_requests, 1);
}

#[test]
fn readers_are_separate_connections() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("readers.db");
    let journal = SqliteEffectJournal::open_with_readers(&path, 3).unwrap();
    let run_id = RunId::new("run-readers");
    let state = AgentState::new(run_id.clone(), BudgetSnapshot::new(2, 2));
    journal.save_run(&state).unwrap();

    assert_eq!(journal.load_run(&run_id).unwrap().as_ref(), Some(&state));
    let health = journal.health(autra_execution::now_unix_ms()).unwrap();
    assert_eq!(health.readers, 3);
    assert!(health.write_queue_capacity >= health.write_queue_active);
}

#[test]
fn effects_can_be_loaded_individually() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("effect-load.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let run_id = RunId::new("run-effect-load");
    let first = authorized_effect();
    let mut second = authorized_effect();
    second.set_status(EffectStatus::Committed);
    journal.record(&run_id, &first).unwrap();
    journal.record(&run_id, &second).unwrap();

    let loaded = journal.load_effect(&run_id, &first.id).unwrap().unwrap();
    assert_eq!(loaded.id, first.id);
    let subset = journal.load_effects(&run_id, &[second.id.clone()]).unwrap();
    assert_eq!(subset.len(), 1);
    assert_eq!(subset[0].id, second.id);
}

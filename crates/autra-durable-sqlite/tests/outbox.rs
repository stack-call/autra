use autra_core::{EffectId, EffectKind, EffectRecord, EffectStatus, RunId};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_runtime::EffectJournal;
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

    journal.release_outbox(&effect.id).unwrap();
    assert_eq!(
        journal.claim_due_effects(10, "claim-2").unwrap()[0].attempts,
        2
    );
    journal.complete_outbox(&effect.id).unwrap();
    assert!(journal.claim_due_effects(10, "claim-3").unwrap().is_empty());
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
        .claim_due_effects_with_lease(10, "claim-1", Duration::ZERO)
        .unwrap();
    assert_eq!(first.len(), 1);
    let blocked = journal
        .claim_due_effects_with_lease(10, "claim-2", Duration::ZERO)
        .unwrap();
    assert!(blocked.is_empty());

    std::thread::sleep(Duration::from_millis(1100));
    let reclaimed = journal
        .claim_due_effects_with_lease(10, "claim-3", Duration::ZERO)
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].attempts, 2);
}

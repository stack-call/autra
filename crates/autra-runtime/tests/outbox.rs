use autra_core::{EffectId, EffectKind, EffectRecord, EffectStatus, RunId};
use autra_runtime::{EffectJournal, MemoryJournal};
use std::time::Duration;

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
fn outbox_enqueue_claim_release_complete() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-1");
    let effect = authorized_effect();

    journal.enqueue_effect(&run_id, &effect).unwrap();
    let claimed = journal.claim_due_effects(10, "claim-1").unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].effect_id, effect.id);
    assert_eq!(claimed[0].attempts, 1);

    journal.release_outbox(&effect.id, "claim-1").unwrap();
    let reclaimed = journal.claim_due_effects(10, "claim-2").unwrap();
    assert_eq!(reclaimed[0].attempts, 2);

    journal.complete_outbox(&effect.id, "claim-2").unwrap();
    assert!(journal.claim_due_effects(10, "claim-3").unwrap().is_empty());
}

#[test]
fn only_authorized_effects_are_enqueued() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-2");
    let effect = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ToolCall,
        None,
        None,
        "digest".to_owned(),
    );

    journal.enqueue_effect(&run_id, &effect).unwrap();

    assert!(journal.claim_due_effects(10, "claim").unwrap().is_empty());
}

#[test]
fn a_stale_claim_cannot_complete_or_release_the_row() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-stale");
    let effect = authorized_effect();

    journal.enqueue_effect(&run_id, &effect).unwrap();
    journal
        .claim_due_effects_with_lease(10, "claim-1", Duration::from_millis(1))
        .unwrap();
    // Simulate lease expiry: a second worker reclaims the same row.
    std::thread::sleep(Duration::from_millis(5));
    let reclaimed = journal
        .claim_due_effects_with_lease(10, "claim-2", Duration::from_millis(1))
        .unwrap();
    assert_eq!(reclaimed.len(), 1);

    journal.complete_outbox(&effect.id, "claim-1").unwrap();
    assert_eq!(
        journal
            .claim_due_effects_with_lease(10, "claim-3", Duration::from_millis(1))
            .unwrap()
            .len(),
        0,
        "a stale claim must not remove the row"
    );

    journal.complete_outbox(&effect.id, "claim-2").unwrap();
    assert!(
        journal
            .claim_due_effects_with_lease(10, "claim-4", Duration::from_millis(1))
            .unwrap()
            .is_empty()
    );
}

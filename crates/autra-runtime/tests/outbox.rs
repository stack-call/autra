use autra_core::{EffectId, EffectKind, EffectRecord, EffectStatus, RunId};
use autra_runtime::{EffectJournal, MemoryJournal};

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

    journal.release_outbox(&effect.id).unwrap();
    let reclaimed = journal.claim_due_effects(10, "claim-2").unwrap();
    assert_eq!(reclaimed[0].attempts, 2);

    journal.complete_outbox(&effect.id).unwrap();
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

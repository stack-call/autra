use std::time::Duration;

use autra_core::{EffectId, RunId, ToolCallRequest};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_execution::{
    ApprovalDecision, ApprovalRecord, ApprovalRequest, ApprovalStore, now_unix_ms,
};
use tempfile::tempdir;

#[test]
fn approval_decisions_survive_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("approvals.db");
    let run_id = RunId::new("run-approval");
    let effect_id = EffectId::fresh();
    let call = ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        arguments: r#"{"path":"note.txt","content":"hello"}"#.to_owned(),
    };
    let request = ApprovalRequest::new(&run_id, &effect_id, &call).with_policy(
        "test-v1",
        Some("operator".to_owned()),
        Some(Duration::from_secs(60)),
    );
    let record = ApprovalRecord::from_request(&request, ApprovalDecision::Approved, now_unix_ms());
    {
        let journal = SqliteEffectJournal::open(&path).unwrap();
        journal.save_approval(&record).unwrap();
    }

    let journal = SqliteEffectJournal::open(&path).unwrap();
    let loaded = journal.load_approval(&effect_id).unwrap().unwrap();

    assert_eq!(loaded, record);
    assert_eq!(loaded.actor.as_deref(), Some("operator"));
    assert!(loaded.is_usable_for(&request, now_unix_ms()));
}

#[test]
fn an_unknown_effect_has_no_approval() {
    let dir = tempdir().unwrap();
    let journal = SqliteEffectJournal::open(dir.path().join("approvals.db")).unwrap();

    assert!(journal.load_approval(&EffectId::fresh()).unwrap().is_none());
}

use autra_core::{HistoryItem, RunId, SessionId, TurnId};
use autra_durable_sqlite::SqliteSessionStore;
use autra_execution::{SessionStore, TurnRecord, TurnStatus};
use tempfile::tempdir;

fn turn(index: u32, input: &str, answer: &str) -> TurnRecord {
    TurnRecord {
        turn_id: TurnId::new(format!("turn-{index}")),
        index,
        run_id: Some(RunId::new(format!("run-{index}"))),
        input: input.to_owned(),
        status: TurnStatus::Completed,
        answer: Some(answer.to_owned()),
        error: None,
        history: vec![
            HistoryItem::User(input.to_owned()),
            HistoryItem::Assistant {
                text: Some(answer.to_owned()),
                tool_calls: Vec::new(),
            },
        ],
    }
}

#[test]
fn turns_survive_reopen_in_order() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let session_id = SessionId::new("session-1");
    {
        let store = SqliteSessionStore::open(&path).unwrap();
        store.create_session(&session_id).unwrap();
        store
            .save_turn(&session_id, &turn(1, "one", "first"))
            .unwrap();
        let mut second = turn(2, "two", "second");
        second.history = vec![
            HistoryItem::User("one".to_owned()),
            HistoryItem::Assistant {
                text: Some("first".to_owned()),
                tool_calls: Vec::new(),
            },
            HistoryItem::User("two".to_owned()),
            HistoryItem::Assistant {
                text: Some("second".to_owned()),
                tool_calls: Vec::new(),
            },
        ];
        store.save_turn(&session_id, &second).unwrap();
    }

    let store = SqliteSessionStore::open(&path).unwrap();
    let record = store.load_session(&session_id).unwrap().unwrap();

    assert_eq!(record.turns.len(), 2);
    assert_eq!(record.turns[0].input, "one");
    assert_eq!(record.turns[1].input, "two");
    assert_eq!(record.history().len(), 4);
    assert_eq!(
        record.last_turn().unwrap().answer.as_deref(),
        Some("second")
    );
}

#[test]
fn saving_a_turn_again_replaces_it() {
    let dir = tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let session_id = SessionId::new("session-2");
    store.create_session(&session_id).unwrap();
    let mut first = turn(1, "one", "first");
    store.save_turn(&session_id, &first).unwrap();
    first.status = TurnStatus::Failed;
    first.answer = None;
    first.error = Some("boom".to_owned());
    store.save_turn(&session_id, &first).unwrap();

    let record = store.load_session(&session_id).unwrap().unwrap();

    assert_eq!(record.turns.len(), 1);
    assert_eq!(record.turns[0].status, TurnStatus::Failed);
    assert_eq!(record.turns[0].error.as_deref(), Some("boom"));
}

#[test]
fn an_unknown_session_is_absent() {
    let dir = tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();

    assert!(
        store
            .load_session(&SessionId::new("missing"))
            .unwrap()
            .is_none()
    );
}

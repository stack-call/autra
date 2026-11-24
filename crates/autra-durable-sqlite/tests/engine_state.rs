use autra::{Agent, EffectJournal};
use autra_core::{ModelOutput, ToolCallRequest};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_harness::{EchoTool, FakeModel};
use tempfile::tempdir;

fn output(text: Option<&str>, calls: Vec<ToolCallRequest>) -> ModelOutput {
    ModelOutput {
        usage: None,
        text: text.map(str::to_owned),
        tool_calls: calls,
    }
}

#[tokio::test]
async fn a_multi_step_run_round_trips_through_snapshot_and_patches() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("engine.db");
    let model = FakeModel::new(vec![
        output(
            None,
            vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: r#"{"text":"hi"}"#.to_owned(),
            }],
        ),
        output(Some("done"), Vec::new()),
    ]);
    let agent = Agent::builder("engine-state")
        .model(model)
        .tool(EchoTool)
        .journal(SqliteEffectJournal::open(&path).unwrap())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let reader = SqliteEffectJournal::open(&path).unwrap();
    let loaded = reader.load_run(&outcome.state().run_id).unwrap().unwrap();
    assert_eq!(&loaded, outcome.state());

    let connection = rusqlite::Connection::open(&path).unwrap();
    let snapshots: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM run_engine_snapshots WHERE run_id = ?1",
            rusqlite::params![outcome.state().run_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let patches: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM run_engine_patches WHERE run_id = ?1",
            rusqlite::params![outcome.state().run_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(snapshots >= 1);
    assert!(patches >= 1);
}

#[tokio::test]
async fn a_corrupt_patch_is_rejected_on_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("corrupt.db");
    let model = FakeModel::new(vec![output(Some("done"), Vec::new())]);
    let agent = Agent::builder("engine-state")
        .model(model)
        .journal(SqliteEffectJournal::open(&path).unwrap())
        .build()
        .unwrap();
    let outcome = agent.run("go").await.unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE run_engine_patches SET patch_json = 'not-json' WHERE run_id = ?1",
            rusqlite::params![outcome.state().run_id.as_str()],
        )
        .unwrap();
    drop(connection);

    let reader = SqliteEffectJournal::open(&path).unwrap();
    let error = reader.load_run(&outcome.state().run_id).unwrap_err();
    assert!(error.to_string().contains("engine patch"));
}

use autra_core::{EffectId, EffectKind, EffectRecord, RunId};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_harness::EchoModel;
use autra_runtime::{Agent, EffectJournal};
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
}

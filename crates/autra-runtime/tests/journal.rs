use autra_harness::EchoModel;
use autra_runtime::{Agent, EffectJournal, MemoryJournal};

#[tokio::test]
async fn a_run_persists_its_effect_records() {
    let journal = MemoryJournal::new();
    let agent = Agent::builder("journal")
        .model(EchoModel)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.run("hello").await.unwrap();

    let stored = journal.load(&outcome.state().run_id).unwrap();
    assert_eq!(stored, outcome.effects());
    assert_eq!(stored.len(), 1);
}

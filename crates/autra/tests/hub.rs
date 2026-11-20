use std::sync::Arc;

use autra::{Agent, RunHub};
use autra_core::AgentPhase;
use autra_harness::EchoModel;

#[tokio::test]
async fn a_run_publishes_a_final_snapshot() {
    let hub = Arc::new(RunHub::new());
    let agent = Agent::builder("hub")
        .model(EchoModel)
        .run_hub(hub.clone())
        .build()
        .unwrap();

    let outcome = agent.run("hi").await.unwrap();
    let run_id = outcome.state().run_id.clone();

    let snapshot = hub.snapshot(&run_id).unwrap();
    assert_eq!(snapshot.phase, AgentPhase::Completed);
    assert_eq!(snapshot.final_answer.as_deref(), Some("echo: hi"));
    assert!(snapshot.last_event_seq > 0);
    assert_eq!(snapshot.effect_count, 1);
    assert_eq!(hub.runs().len(), 1);
    let subscription = hub.subscribe(&run_id).unwrap();
    assert_eq!(subscription.snapshot, snapshot);
}

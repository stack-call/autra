use std::sync::Arc;

use autra_core::{AgentPhase, BudgetSnapshot, RunId};
use autra_execution::{RunEvent, RunSnapshot};
use autra_runtime::RunHub;

fn snapshot(run_id: &RunId, phase: AgentPhase, seq: u64) -> RunSnapshot {
    RunSnapshot {
        run_id: run_id.clone(),
        parent_run_id: None,
        phase,
        budget: BudgetSnapshot::new(4, 4),
        final_answer: None,
        last_error: None,
        last_event_seq: seq,
        effect_count: 0,
    }
}

#[tokio::test]
async fn a_subscription_receives_events_and_ends_when_the_run_finishes() {
    let hub = Arc::new(RunHub::new());
    let run_id = RunId::new("run-hub");
    hub.start(snapshot(&run_id, AgentPhase::Ready, 0));
    let mut subscription = hub.subscribe(&run_id).unwrap();

    hub.publish(&run_id, &RunEvent::ModelCallStarted { turn: 1 });
    let event = subscription.recv().await.unwrap();
    assert!(matches!(event, RunEvent::ModelCallStarted { turn: 1 }));

    hub.finish(snapshot(&run_id, AgentPhase::Completed, 3));
    assert!(subscription.recv().await.is_none());
    assert_eq!(hub.snapshot(&run_id).unwrap().phase, AgentPhase::Completed);
    assert_eq!(hub.snapshot(&run_id).unwrap().last_event_seq, 3);
}

#[tokio::test]
async fn subscribing_after_finish_returns_the_snapshot_without_a_live_stream() {
    let hub = RunHub::new();
    let run_id = RunId::new("run-finished");
    hub.start(snapshot(&run_id, AgentPhase::Ready, 0));
    hub.finish(snapshot(&run_id, AgentPhase::Cancelled, 2));

    let mut subscription = hub.subscribe(&run_id).unwrap();

    assert_eq!(subscription.snapshot.phase, AgentPhase::Cancelled);
    assert!(subscription.recv().await.is_none());
}

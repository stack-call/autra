use std::sync::{Arc, Mutex};

use autra_core::{AgentPhase, EffectIntent, RunId};
use autra_harness::EchoModel;
use autra_runtime::{Agent, ObserverHook};

struct RecordingHook {
    events: Arc<Mutex<Vec<String>>>,
}

impl RecordingHook {
    fn push(&self, event: String) {
        self.events.lock().expect("hook log poisoned").push(event);
    }
}

impl ObserverHook for RecordingHook {
    fn on_run_started(&self, _run_id: &RunId) {
        self.push("started".to_owned());
    }

    fn on_model_request(&self, _run_id: &RunId, turn: u32) {
        self.push(format!("model:{turn}"));
    }

    fn on_effect_accepted(&self, _run_id: &RunId, effect: &EffectIntent) {
        self.push(format!("effect:{effect:?}"));
    }

    fn on_run_finished(&self, _run_id: &RunId, phase: &AgentPhase) {
        self.push(format!("finished:{phase:?}"));
    }
}

#[tokio::test]
async fn observer_hooks_see_the_run_lifecycle() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder("hooks")
        .model(EchoModel)
        .observer(RecordingHook {
            events: events.clone(),
        })
        .build()
        .unwrap();

    agent.run("hello").await.unwrap();

    let events = events.lock().unwrap();
    assert!(events.iter().any(|event| event == "started"));
    assert!(events.iter().any(|event| event.starts_with("model:")));
    assert!(events.iter().any(|event| event.starts_with("effect:")));
    assert!(events.iter().any(|event| event == "finished:Completed"));
}

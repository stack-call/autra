use std::sync::{Arc, Mutex};

use autra::{Agent, ObserverHook};
use autra_core::{AgentPhase, EffectIntent, EffectRecord, ModelOutput, RunId, ToolCallRequest};
use autra_harness::{EchoModel, EchoTool, FakeModel};

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

struct OrderingHook {
    events: Arc<Mutex<Vec<String>>>,
}

impl OrderingHook {
    fn push(&self, event: impl Into<String>) {
        self.events
            .lock()
            .expect("hook log poisoned")
            .push(event.into());
    }
}

impl ObserverHook for OrderingHook {
    fn on_effect_proposed(&self, _run_id: &RunId, effect: &EffectIntent) {
        let kind = match effect {
            EffectIntent::CallModel(_) => "model",
            EffectIntent::CallTool(_) => "tool",
        };
        self.push(format!("proposed:{kind}"));
    }

    fn on_effect_committed(&self, _run_id: &RunId, effect: &EffectRecord) {
        self.push(format!("committed:{:?}", effect.kind));
    }

    fn on_effect_authorized(&self, _run_id: &RunId, effect: &EffectRecord) {
        self.push(format!("authorized:{:?}", effect.kind));
    }

    fn on_effect_resolved(&self, _run_id: &RunId, effect: &EffectRecord) {
        self.push(format!("resolved:{:?}", effect.kind));
    }

    fn on_run_finished(&self, _run_id: &RunId, _phase: &AgentPhase) {
        self.push("finished");
    }
}

#[tokio::test]
async fn effect_lifecycle_is_ordered_around_commit_and_authorization() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let model = FakeModel::new(vec![
        ModelOutput {
            usage: None,
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: r#"{"text":"hi"}"#.to_owned(),
            }],
        },
        ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = Agent::builder("hooks")
        .model(model)
        .tool(EchoTool)
        .observer(OrderingHook {
            events: events.clone(),
        })
        .build()
        .unwrap();

    agent.run("go").await.unwrap();

    let events = events.lock().unwrap();
    let index = |label: &str| {
        events
            .iter()
            .position(|event| event == label)
            .unwrap_or_else(|| panic!("missing {label} in {events:?}"))
    };
    assert!(index("proposed:model") < index("committed:ModelCall"));
    assert!(index("committed:ModelCall") < index("resolved:ModelCall"));
    assert!(index("proposed:tool") < index("committed:ToolCall"));
    assert!(index("committed:ToolCall") < index("authorized:ToolCall"));
    assert!(index("authorized:ToolCall") < index("resolved:ToolCall"));
    assert!(index("resolved:ToolCall") < index("finished"));
}

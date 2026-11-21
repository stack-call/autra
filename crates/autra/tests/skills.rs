use autra::react_history;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra::Agent;
use autra_core::{ModelCallSpec, ModelOutput, ToolCallRequest};
use autra_harness::{HistoryItem, ModelError, ModelProvider, Skill};

struct CapturingModel {
    responses: Mutex<VecDeque<ModelOutput>>,
    systems: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl ModelProvider for CapturingModel {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.systems
            .lock()
            .expect("system log poisoned")
            .push(request.system.clone());
        self.responses
            .lock()
            .expect("response queue poisoned")
            .pop_front()
            .ok_or_else(|| ModelError::new("capturing model exhausted"))
    }
}

#[tokio::test]
async fn agent_can_load_a_named_skill() {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let model = CapturingModel {
        responses: Mutex::new(VecDeque::from(vec![
            ModelOutput {
                usage: None,
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "load_skill".to_owned(),
                    arguments: r#"{"name":"rust"}"#.to_owned(),
                }],
            },
            ModelOutput {
                usage: None,
                text: Some("done".to_owned()),
                tool_calls: Vec::new(),
            },
        ])),
        systems: systems.clone(),
    };
    let agent = Agent::builder("skills")
        .model(model)
        .skill(Skill::new(
            "rust",
            "Rust guidance",
            "Run cargo fmt before reporting.",
        ))
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let history = react_history(outcome.state()).unwrap();
    let tool = history
        .iter()
        .find_map(|item| match item {
            HistoryItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .unwrap();
    assert!(tool.content.contains("Run cargo fmt before reporting."));
    let systems = systems.lock().unwrap();
    let system = systems[0].as_deref().unwrap();
    assert!(system.contains("Available skills"));
    assert!(system.contains("rust: Rust guidance"));
}

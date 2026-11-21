use autra::react_history;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra::Agent;
use autra_core::{ModelCallSpec, ModelOutput, ToolCallRequest};
use autra_harness::{HistoryItem, ModelError, ModelProvider, PlanStatus};

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
async fn update_plan_populates_the_agent_plan() {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let model = CapturingModel {
        responses: Mutex::new(VecDeque::from(vec![
            ModelOutput {
                usage: None,
                text: None,
                tool_calls: vec![ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "update_plan".to_owned(),
                    arguments: r#"{"items":[{"title":"inspect","status":"completed"},{"title":"edit","status":"in_progress"}]}"#.to_owned(),
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
    let agent = Agent::builder("planner")
        .model(model)
        .planning()
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let plan = agent.plan().unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].title, "inspect");
    assert_eq!(plan[0].status, PlanStatus::Completed);
    assert_eq!(plan[1].title, "edit");
    assert_eq!(plan[1].status, PlanStatus::InProgress);
    let history = react_history(outcome.state()).unwrap();
    let tool = history
        .iter()
        .find_map(|item| match item {
            HistoryItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .unwrap();
    assert!(tool.content.contains("- [x] inspect"));
    assert!(tool.content.contains("- [~] edit"));
    let systems = systems.lock().unwrap();
    assert!(systems[0].as_deref().unwrap().contains("update_plan"));
}

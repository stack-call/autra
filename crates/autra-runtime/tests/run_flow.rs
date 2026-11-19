use async_trait::async_trait;
use autra_core::{ModelCallSpec, ModelOutput, ToolCallRequest};
use autra_harness::{EchoModel, ModelError, ModelProvider};
use autra_runtime::{Agent, RunEvent, RunOptions, RunStatus};

#[tokio::test]
async fn echo_agent_returns_the_echoed_answer() {
    let agent = Agent::builder("echo-agent")
        .instructions("be brief")
        .model(EchoModel)
        .build()
        .unwrap();

    let mut events = Vec::new();
    let outcome = agent
        .run_with_options("hello", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("echo: hello"));
    assert_eq!(outcome.turns_used(), 1);
    assert_eq!(outcome.tool_calls_used(), 0);
    assert_eq!(
        event_names(&events),
        [
            "RunStarted",
            "ModelCallStarted",
            "ModelResult",
            "RunCompleted"
        ]
    );
}

#[test]
fn building_without_a_model_fails() {
    let error = match Agent::builder("no-model").build() {
        Ok(_) => panic!("expected a missing-model error"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("requires a model"));
}

fn event_names(events: &[RunEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            RunEvent::RunStarted { .. } => "RunStarted",
            RunEvent::ModelCallStarted { .. } => "ModelCallStarted",
            RunEvent::ModelResult { .. } => "ModelResult",
            RunEvent::ToolCallStarted { .. } => "ToolCallStarted",
            RunEvent::ToolResult { .. } => "ToolResult",
            RunEvent::RunCompleted { .. } => "RunCompleted",
            RunEvent::RunCancelled => "RunCancelled",
            RunEvent::RunFailed { .. } => "RunFailed",
        })
        .collect()
}

struct FailingModel;

#[async_trait]
impl ModelProvider for FailingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Err(ModelError::new("provider unavailable"))
    }
}

struct ToolCallingModel;

#[async_trait]
impl ModelProvider for ToolCallingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Ok(ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            }],
        })
    }
}

#[tokio::test]
async fn model_failure_emits_run_failed() {
    let agent = Agent::builder("failing")
        .model(FailingModel)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let error = agent
        .run_with_options("hello", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("provider unavailable"));
    assert_eq!(event_names(&events).last(), Some(&"RunFailed"));
}

#[tokio::test]
async fn protocol_failure_emits_run_failed() {
    let agent = Agent::builder("tool-calling")
        .model(ToolCallingModel)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let error = agent
        .run_with_options("hello", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("tool calls are not supported"));
    assert_eq!(event_names(&events).last(), Some(&"RunFailed"));
}

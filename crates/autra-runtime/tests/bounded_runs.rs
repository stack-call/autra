use std::time::Duration;

use async_trait::async_trait;
use autra_core::{CancelFlag, ModelCallSpec, ModelOutput, ToolCallRequest, ToolSpec};
use autra_harness::{EchoTool, FakeModel, ModelError, ModelProvider, Tool, ToolError};
use autra_runtime::{Agent, RunEvent, RunOptions, RunStatus};

fn call(id: &str, name: &str, arguments: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn final_answer(text: &str) -> ModelOutput {
    ModelOutput {
        text: Some(text.to_owned()),
        tool_calls: Vec::new(),
    }
}

fn tool_call(call: ToolCallRequest) -> ModelOutput {
    ModelOutput {
        text: None,
        tool_calls: vec![call],
    }
}

fn event_names(events: &[RunEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            RunEvent::RunStarted { .. } => "RunStarted",
            RunEvent::ModelCallStarted { .. } => "ModelCallStarted",
            RunEvent::ModelResult { .. } => "ModelResult",
            RunEvent::ApprovalRequested { .. } => "ApprovalRequested",
            RunEvent::ApprovalDecided { .. } => "ApprovalDecided",
            RunEvent::ToolCallStarted { .. } => "ToolCallStarted",
            RunEvent::ToolResult { .. } => "ToolResult",
            RunEvent::RunCompleted { .. } => "RunCompleted",
            RunEvent::RunCancelled => "RunCancelled",
            RunEvent::RunFailed { .. } => "RunFailed",
            RunEvent::RunResumed { .. } => "RunResumed",
        })
        .collect()
}

#[tokio::test]
async fn cancellation_before_first_effect_skips_the_model_call() {
    let model = FakeModel::new(Vec::new());
    let agent = Agent::builder("cancel-first").model(model).build().unwrap();
    let options = RunOptions::default();
    options.cancel.cancel();
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", options, |event| events.push(event.clone()))
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
    assert_eq!(event_names(&events).last(), Some(&"RunCancelled"));
}

#[tokio::test]
async fn max_turns_fails_the_run() {
    let model = FakeModel::new(vec![tool_call(call("call-1", "echo", r#"{"text":"x"}"#))]);
    let agent = Agent::builder("max-turns")
        .model(model)
        .tool(EchoTool)
        .max_turns(1)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let error = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("max turns exceeded"));
    assert_eq!(event_names(&events).last(), Some(&"RunFailed"));
}

#[tokio::test]
async fn max_tool_calls_fails_before_executing_any_tool() {
    let model = FakeModel::new(vec![ModelOutput {
        text: None,
        tool_calls: vec![
            call("call-1", "echo", r#"{"text":"one"}"#),
            call("call-2", "echo", r#"{"text":"two"}"#),
        ],
    }]);
    let agent = Agent::builder("max-tools")
        .model(model)
        .tool(EchoTool)
        .max_tool_calls(1)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let error = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("max tool calls exceeded"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RunEvent::ToolCallStarted { .. }))
    );
}

struct SlowModel;

#[async_trait]
impl ModelProvider for SlowModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(final_answer("too late"))
    }
}

#[tokio::test]
async fn model_timeout_fails_the_run() {
    let agent = Agent::builder("slow-model")
        .model(SlowModel)
        .build()
        .unwrap();
    let options = RunOptions {
        effect_timeout: Some(Duration::from_millis(10)),
        ..RunOptions::default()
    };
    let mut events = Vec::new();

    let error = agent
        .run_with_options("go", options, |event| events.push(event.clone()))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("model call exceeded"));
    assert_eq!(event_names(&events).last(), Some(&"RunFailed"));
}

struct SlowTool;

#[async_trait]
impl Tool for SlowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "slow".to_owned(),
            description: "Sleeps past the timeout.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok("too late".to_owned())
    }
}

#[tokio::test]
async fn tool_timeout_becomes_an_error_outcome() {
    let model = FakeModel::new(vec![
        tool_call(call("call-1", "slow", "{}")),
        final_answer("recovered"),
    ]);
    let agent = Agent::builder("slow-tool")
        .model(model)
        .tool(SlowTool)
        .build()
        .unwrap();
    let options = RunOptions {
        effect_timeout: Some(Duration::from_millis(10)),
        ..RunOptions::default()
    };
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", options, |event| events.push(event.clone()))
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("recovered"));
    let (content, is_error, outcome_unknown) = events
        .iter()
        .find_map(|event| match event {
            RunEvent::ToolResult {
                content,
                is_error,
                outcome_unknown,
                ..
            } => Some((content.clone(), *is_error, *outcome_unknown)),
            _ => None,
        })
        .unwrap();
    assert!(is_error);
    assert!(outcome_unknown);
    assert!(content.contains("timed out"));
}

#[tokio::test]
async fn run_options_timeout_overrides_builder_timeout() {
    let agent = Agent::builder("builder-timeout")
        .model(SlowModel)
        .effect_timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let options = RunOptions {
        effect_timeout: Some(Duration::from_millis(10)),
        ..RunOptions::default()
    };

    let error = agent
        .run_with_options("go", options, |_| {})
        .await
        .unwrap_err();

    assert!(error.to_string().contains("model call exceeded"));
}

#[tokio::test]
async fn default_max_turns_is_eight() {
    let model = FakeModel::new(
        (0..8).map(|index| tool_call(call(&format!("call-{index}"), "echo", r#"{"text":"x"}"#))),
    );
    let agent = Agent::builder("default-turns")
        .model(model)
        .tool(EchoTool)
        .max_tool_calls(100)
        .build()
        .unwrap();

    let error = agent.run("go").await.unwrap_err();

    assert!(error.to_string().contains("max turns exceeded"));
}

#[tokio::test]
async fn default_max_tool_calls_is_eight() {
    let model = FakeModel::new(vec![ModelOutput {
        text: None,
        tool_calls: (0..9)
            .map(|index| call(&format!("call-{index}"), "echo", r#"{"text":"x"}"#))
            .collect(),
    }]);
    let agent = Agent::builder("default-tools")
        .model(model)
        .tool(EchoTool)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let error = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("max tool calls exceeded"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RunEvent::ToolCallStarted { .. }))
    );
}

#[tokio::test]
async fn builder_effect_timeout_applies_with_default_options() {
    let agent = Agent::builder("builder-timeout")
        .model(SlowModel)
        .effect_timeout(Duration::from_millis(10))
        .build()
        .unwrap();

    let error = agent.run("go").await.unwrap_err();

    assert!(error.to_string().contains("model call exceeded"));
}

struct CancellingModel {
    cancel: CancelFlag,
}

#[async_trait]
impl ModelProvider for CancellingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.cancel.cancel();
        Ok(final_answer("finished"))
    }
}

#[tokio::test]
async fn cancel_during_final_model_call_completes_the_run() {
    let cancel = CancelFlag::new();
    let agent = Agent::builder("cancel-final")
        .model(CancellingModel {
            cancel: cancel.clone(),
        })
        .build()
        .unwrap();
    let options = RunOptions {
        cancel: cancel.clone(),
        ..RunOptions::default()
    };

    let outcome = agent.run_with_options("go", options, |_| {}).await.unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("finished"));
}

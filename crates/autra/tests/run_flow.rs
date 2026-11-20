use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use autra::{Agent, RunEvent, RunOptions, RunStatus};
use autra_core::{CancelFlag, ModelCallSpec, ModelOutput, ToolCallRequest, ToolSpec};
use autra_harness::{EchoModel, EchoTool, FakeModel, ModelError, ModelProvider, Tool, ToolError};

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

struct FailingModel;

#[async_trait]
impl ModelProvider for FailingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Err(ModelError::new("provider unavailable"))
    }
}

struct DuplicateCallModel;

#[async_trait]
impl ModelProvider for DuplicateCallModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        let call = ToolCallRequest {
            call_id: "call-1".to_owned(),
            name: "echo".to_owned(),
            arguments: "{}".to_owned(),
        };
        Ok(ModelOutput {
            text: None,
            tool_calls: vec![call.clone(), call],
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
    let agent = Agent::builder("duplicate-calls")
        .model(DuplicateCallModel)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let error = agent
        .run_with_options("hello", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap_err();

    assert!(error.to_string().contains("duplicate tool call id"));
    assert_eq!(event_names(&events).last(), Some(&"RunFailed"));
}

fn call(id: &str, name: &str, arguments: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn tool_call_model(calls: Vec<ToolCallRequest>, answer: &str) -> FakeModel {
    FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: calls,
        },
        ModelOutput {
            text: Some(answer.to_owned()),
            tool_calls: Vec::new(),
        },
    ])
}

fn tool_result(events: &[RunEvent]) -> (String, bool) {
    events
        .iter()
        .find_map(|event| match event {
            RunEvent::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .expect("expected a tool result event")
}

#[tokio::test]
async fn tool_calls_are_executed_and_fed_back() {
    let model = tool_call_model(
        vec![call("call-1", "echo", r#"{"text":"hello"}"#)],
        "tool said hello",
    );
    let agent = Agent::builder("tools")
        .model(model)
        .tool(EchoTool)
        .build()
        .unwrap();

    let mut events = Vec::new();
    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("tool said hello"));
    assert_eq!(outcome.tool_calls_used(), 1);
    assert_eq!(
        event_names(&events),
        [
            "RunStarted",
            "ModelCallStarted",
            "ModelResult",
            "ToolCallStarted",
            "ToolResult",
            "ModelCallStarted",
            "ModelResult",
            "RunCompleted"
        ]
    );
    assert_eq!(tool_result(&events), ("hello".to_owned(), false));
}

#[tokio::test]
async fn multiple_tool_calls_execute_in_model_order() {
    let model = tool_call_model(
        vec![
            call("call-1", "echo", r#"{"text":"one"}"#),
            call("call-2", "echo", r#"{"text":"two"}"#),
        ],
        "both done",
    );
    let agent = Agent::builder("multi-tools")
        .model(model)
        .tool(EchoTool)
        .build()
        .unwrap();

    let mut events = Vec::new();
    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("both done"));
    assert_eq!(outcome.tool_calls_used(), 2);
    let names = event_names(&events);
    assert_eq!(
        &names[3..7],
        [
            "ToolCallStarted",
            "ToolResult",
            "ToolCallStarted",
            "ToolResult"
        ]
    );
}

#[tokio::test]
async fn unknown_tool_becomes_an_error_outcome() {
    let model = tool_call_model(vec![call("call-1", "missing", "{}")], "recovered");
    let agent = Agent::builder("unknown-tool")
        .model(model)
        .tool(EchoTool)
        .build()
        .unwrap();

    let mut events = Vec::new();
    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("recovered"));
    let (content, is_error) = tool_result(&events);
    assert!(is_error);
    assert!(content.contains("unknown tool"));
}

#[tokio::test]
async fn malformed_tool_arguments_become_an_error_outcome() {
    let model = tool_call_model(vec![call("call-1", "echo", "{}")], "recovered");
    let agent = Agent::builder("bad-args")
        .model(model)
        .tool(EchoTool)
        .build()
        .unwrap();

    let mut events = Vec::new();
    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("recovered"));
    let (content, is_error) = tool_result(&events);
    assert!(is_error);
    assert!(content.contains("invalid echo arguments"));
}

struct CancellingTool {
    calls: Arc<AtomicUsize>,
    cancel: CancelFlag,
}

#[async_trait]
impl Tool for CancellingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cancel".to_owned(),
            description: "Cancel the run after recording a call.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> autra_core::EffectClass {
        autra_core::EffectClass::ReadOnly
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.cancel.cancel();
        Ok("cancelled".to_owned())
    }
}

#[tokio::test]
async fn cancellation_stops_remaining_tool_intents() {
    let cancel = CancelFlag::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let model = FakeModel::new(vec![ModelOutput {
        text: None,
        tool_calls: vec![
            call("call-1", "cancel", "{}"),
            call("call-2", "cancel", "{}"),
        ],
    }]);
    let agent = Agent::builder("cancel-batch")
        .model(model)
        .tool(CancellingTool {
            calls: calls.clone(),
            cancel: cancel.clone(),
        })
        .build()
        .unwrap();
    let options = RunOptions {
        cancel: cancel.clone(),
        ..RunOptions::default()
    };

    let mut events = Vec::new();
    let outcome = agent
        .run_with_options("go", options, |event| events.push(event.clone()))
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        events.iter().any(
            |event| matches!(event, RunEvent::ToolResult { call_id, .. } if call_id == "call-1")
        )
    );
    assert_eq!(outcome.tool_calls_used(), 2);
    assert_eq!(event_names(&events).last(), Some(&"RunCancelled"));
}

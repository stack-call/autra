use autra_harness::EchoModel;
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

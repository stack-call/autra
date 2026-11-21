use autra::react_history;
use autra::{Agent, RunEvent, RunOptions, RunStatus, SubagentRegistry, SubagentTool};
use autra_core::{ModelOutput, ToolCallRequest};
use autra_harness::{FakeModel, HistoryItem, Tool};

fn child(name: &str, answer: &str) -> Agent {
    Agent::builder(name)
        .model(FakeModel::new(vec![ModelOutput {
            text: Some(answer.to_owned()),
            tool_calls: Vec::new(),
        }]))
        .build()
        .unwrap()
}

fn spawn_call(id: &str, agent: &str, task: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: id.to_owned(),
        name: "spawn_subagent".to_owned(),
        arguments: serde_json::json!({ "agent": agent, "task": task }).to_string(),
    }
}

fn parent_model(calls: Vec<ToolCallRequest>) -> FakeModel {
    FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: calls,
        },
        ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        },
    ])
}

fn tool_outcomes(outcome: &autra_runtime::RunOutcome) -> Vec<autra_core::ToolOutcome> {
    react_history(outcome.state())
        .into_iter()
        .filter_map(|item| match item {
            HistoryItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn parent_receives_the_child_answer() {
    let parent = Agent::builder("parent")
        .model(parent_model(vec![spawn_call(
            "call-1",
            "researcher",
            "find the failing test",
        )]))
        .subagent(
            "researcher",
            "Finds facts",
            child("researcher", "the test is flaky"),
        )
        .build()
        .unwrap();
    let mut events = Vec::new();

    let outcome = parent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("done"));
    let tools = tool_outcomes(&outcome);
    assert_eq!(tools.len(), 1);
    assert!(!tools[0].is_error);
    assert_eq!(tools[0].content, "the test is flaky");
    assert!(events.iter().any(
        |event| matches!(event, RunEvent::ToolResult { name, .. } if name == "spawn_subagent")
    ));
}

#[tokio::test]
async fn unknown_subagent_becomes_an_error_outcome() {
    let parent = Agent::builder("parent")
        .model(parent_model(vec![spawn_call(
            "call-1", "missing", "do work",
        )]))
        .subagent("researcher", "Finds facts", child("researcher", "unused"))
        .build()
        .unwrap();

    let outcome = parent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let tools = tool_outcomes(&outcome);
    assert!(tools[0].is_error);
    assert!(tools[0].content.contains("unknown subagent"));
}

#[tokio::test]
async fn child_failure_becomes_an_error_outcome() {
    let broken = Agent::builder("broken")
        .model(FakeModel::new(Vec::new()))
        .build()
        .unwrap();
    let parent = Agent::builder("parent")
        .model(parent_model(vec![spawn_call(
            "call-1", "broken", "do work",
        )]))
        .subagent("broken", "Always fails", broken)
        .build()
        .unwrap();

    let outcome = parent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let tools = tool_outcomes(&outcome);
    assert!(tools[0].is_error);
    assert!(tools[0].content.contains("subagent `broken` failed"));
}

#[test]
fn schema_lists_registered_subagents() {
    let mut registry = SubagentRegistry::new();
    registry.register("researcher", "Finds facts", child("researcher", "answer"));
    let tool = SubagentTool::new(registry);

    let spec = tool.spec();

    assert!(spec.input_schema.contains("researcher"));
    assert!(spec.description.contains("Finds facts"));
}

#[tokio::test]
async fn spawn_subagent_reserves_child_tools_from_the_parent() {
    let parent = Agent::builder("parent")
        .model(parent_model(vec![spawn_call(
            "call-1",
            "researcher",
            "find facts",
        )]))
        .subagent("researcher", "Finds facts", child("researcher", "fact"))
        .build()
        .unwrap();

    let outcome = parent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    // The parent spends one call on spawn_subagent and reserves four more for
    // the child budget, so a subagent cannot amplify the parent allowance.
    assert_eq!(outcome.state().budget.tool_calls_used, 5);
    assert_eq!(outcome.state().budget.turns_used, 3);
}

#[tokio::test]
async fn a_child_with_no_remaining_budget_cannot_use_tools() {
    let child_model = FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "child-call".to_owned(),
                name: "echo".to_owned(),
                arguments: r#"{"text":"hi"}"#.to_owned(),
            }],
        },
        ModelOutput {
            text: Some("child done".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let child = Agent::builder("child")
        .model(child_model)
        .tool(autra_harness::EchoTool)
        .build()
        .unwrap();
    let parent = Agent::builder("parent")
        .model(parent_model(vec![spawn_call("call-1", "child", "do work")]))
        .subagent("child", "Uses a tool", child)
        .max_tool_calls(1)
        .build()
        .unwrap();

    let outcome = parent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let tools = tool_outcomes(&outcome);
    assert!(tools[0].is_error);
    assert!(tools[0].content.contains("max tool calls exceeded"));
}

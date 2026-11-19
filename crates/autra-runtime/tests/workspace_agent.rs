use std::fs;

use autra_core::{ModelOutput, ToolCallRequest};
use autra_harness::{FakeModel, ListFilesTool, LocalWorkspace, ReadFileTool};
use autra_runtime::{Agent, RunEvent, RunOptions, RunStatus};
use tempfile::tempdir;

#[tokio::test]
async fn agent_lists_and_reads_a_workspace_file() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "# Demo\nhello").unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let model = FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "list_files".to_owned(),
                arguments: r#"{"path":"."}"#.to_owned(),
            }],
        },
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-2".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"README.md"}"#.to_owned(),
            }],
        },
        ModelOutput {
            text: Some("read the readme".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = Agent::builder("workspace-agent")
        .model(model)
        .tool(ListFilesTool::new(workspace.clone()))
        .tool(ReadFileTool::new(workspace))
        .build()
        .unwrap();

    let mut tool_contents = Vec::new();
    let outcome = agent
        .run_with_options("inspect", RunOptions::default(), |event| {
            if let RunEvent::ToolResult { content, .. } = event {
                tool_contents.push(content.clone());
            }
        })
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.answer(), Some("read the readme"));
    assert_eq!(outcome.tool_calls_used(), 2);
    assert!(
        tool_contents
            .iter()
            .any(|content| content.contains("README.md"))
    );
    assert!(
        tool_contents
            .iter()
            .any(|content| content.contains("hello"))
    );
}

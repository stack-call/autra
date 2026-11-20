use std::fs;

use autra::{Agent, RunEvent, RunOptions, RunStatus};
use autra_core::{ModelOutput, ToolCallRequest};
use autra_harness::{FakeModel, ListFilesTool, LocalWorkspace, ReadFileTool};
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

struct AutoApprove;

#[async_trait::async_trait]
impl autra_runtime::Approver for AutoApprove {
    async fn approve(
        &self,
        _request: &autra_runtime::ApprovalRequest,
    ) -> autra_runtime::ApprovalDecision {
        autra_runtime::ApprovalDecision::Approved
    }
}

#[tokio::test]
async fn agent_writes_a_nested_file_and_runs_a_command() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let model = FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "write_file".to_owned(),
                arguments: r#"{"path":"notes/out.txt","content":"hello"}"#.to_owned(),
            }],
        },
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-2".to_owned(),
                name: "run_command".to_owned(),
                arguments: r#"{"command":"echo done > marker.txt"}"#.to_owned(),
            }],
        },
        ModelOutput {
            text: Some("workspace updated".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = Agent::builder("workspace-agent")
        .model(model)
        .tool(autra_harness::WriteFileTool::new(workspace.clone()))
        .tool(autra_harness::RunCommandTool::new(workspace))
        .approver(AutoApprove)
        .build()
        .unwrap();
    let mut approvals = 0_usize;

    let outcome = agent
        .run_with_options("update", RunOptions::default(), |event| {
            if matches!(event, RunEvent::ApprovalDecided { approved: true, .. }) {
                approvals += 1;
            }
        })
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Completed);
    assert_eq!(outcome.tool_calls_used(), 2);
    assert_eq!(approvals, 2);
    assert_eq!(
        fs::read_to_string(dir.path().join("notes/out.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("marker.txt"))
            .unwrap()
            .trim(),
        "done"
    );
}

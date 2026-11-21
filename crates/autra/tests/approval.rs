use autra::react_history;
use std::fs;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra::{Agent, ApprovalDecision, ApprovalRequest, Approver, RunEvent, RunOptions, RunStatus};
use autra_core::{ModelOutput, ToolCallRequest};
use autra_harness::{EchoTool, FakeModel, HistoryItem, LocalWorkspace, WriteFileTool};
use tempfile::tempdir;

struct ScriptedApprover {
    decision: ApprovalDecision,
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
}

#[async_trait]
impl Approver for ScriptedApprover {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.seen
            .lock()
            .expect("approval log poisoned")
            .push(request.clone());
        self.decision
    }
}

struct AlwaysApprove;

#[async_trait]
impl Approver for AlwaysApprove {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Approved
    }
}

struct AlwaysReject;

#[async_trait]
impl Approver for AlwaysReject {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Rejected
    }
}

fn write_call(id: &str, path: &str, content: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: id.to_owned(),
        name: "write_file".to_owned(),
        arguments: serde_json::json!({ "path": path, "content": content }).to_string(),
    }
}

fn model_with(calls: Vec<ToolCallRequest>) -> FakeModel {
    FakeModel::new(vec![
        ModelOutput {
            usage: None,
            text: None,
            tool_calls: calls,
        },
        ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        },
    ])
}

fn tool_outcomes(outcome: &autra_runtime::RunOutcome) -> Vec<autra_core::ToolOutcome> {
    react_history(outcome.state())
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            HistoryItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn approved_write_executes_and_binds_the_request() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder("approved")
        .model(model_with(vec![write_call("call-1", "note.txt", "hello")]))
        .tool(WriteFileTool::new(workspace))
        .approver(ScriptedApprover {
            decision: ApprovalDecision::Approved,
            seen: seen.clone(),
        })
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
    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].tool_name, "write_file");
    assert_eq!(seen[0].policy_revision, "local-v1");
    assert_eq!(seen[0].argument_digest.len(), 64);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunEvent::ApprovalRequested { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunEvent::ApprovalDecided { approved: true, .. }))
    );
}

#[tokio::test]
async fn rejected_write_does_not_touch_the_workspace() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder("rejected")
        .model(model_with(vec![write_call("call-1", "note.txt", "hello")]))
        .tool(WriteFileTool::new(workspace))
        .approver(ScriptedApprover {
            decision: ApprovalDecision::Rejected,
            seen: seen.clone(),
        })
        .build()
        .unwrap();
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert!(!dir.path().join("note.txt").exists());
    let tools = tool_outcomes(&outcome);
    assert_eq!(tools.len(), 1);
    assert!(tools[0].is_error);
    assert!(tools[0].content.contains("approval rejected"));
    assert!(events.iter().any(|event| matches!(
        event,
        RunEvent::ApprovalDecided {
            approved: false,
            ..
        }
    )));
}

#[tokio::test]
async fn missing_approver_rejects_the_write() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let agent = Agent::builder("no-approver")
        .model(model_with(vec![write_call("call-1", "note.txt", "hello")]))
        .tool(WriteFileTool::new(workspace))
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert!(!dir.path().join("note.txt").exists());
    assert!(
        tool_outcomes(&outcome)[0]
            .content
            .contains("approval rejected")
    );
}

#[tokio::test]
async fn run_options_approver_overrides_the_builder() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let agent = Agent::builder("override")
        .model(model_with(vec![write_call("call-1", "note.txt", "hello")]))
        .tool(WriteFileTool::new(workspace))
        .approver(AlwaysReject)
        .build()
        .unwrap();
    let options = RunOptions {
        approver: Some(Arc::new(AlwaysApprove)),
        ..RunOptions::default()
    };

    let outcome = agent.run_with_options("go", options, |_| {}).await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn mixed_batch_approves_only_the_write() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let model = model_with(vec![
        write_call("call-1", "note.txt", "hello"),
        ToolCallRequest {
            call_id: "call-2".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"text":"read-only"}"#.to_owned(),
        },
    ]);
    let agent = Agent::builder("mixed")
        .model(model)
        .tool(WriteFileTool::new(workspace))
        .tool(EchoTool)
        .approver(AlwaysApprove)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(outcome.tool_calls_used(), 2);
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
    let approval_requests = events
        .iter()
        .filter(|event| matches!(event, RunEvent::ApprovalRequested { .. }))
        .count();
    assert_eq!(approval_requests, 1);
    let tools = tool_outcomes(&outcome);
    assert_eq!(tools.len(), 2);
    assert!(tools.iter().all(|tool| !tool.is_error));
}

#[tokio::test]
async fn approval_carries_a_tool_preview() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder("preview")
        .model(model_with(vec![write_call("call-1", "note.txt", "hello")]))
        .tool(WriteFileTool::new(workspace))
        .approver(ScriptedApprover {
            decision: ApprovalDecision::Approved,
            seen: seen.clone(),
        })
        .build()
        .unwrap();

    agent.run("go").await.unwrap();

    let seen = seen.lock().unwrap();
    let preview = seen[0].preview.as_deref().expect("tool preview");
    assert!(preview.contains("create note.txt"));
    assert!(preview.contains("hello"));
}

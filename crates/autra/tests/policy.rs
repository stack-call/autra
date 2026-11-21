use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use autra::{Agent, ApprovalDecision, ApprovalRequest, Approver, RulePolicy, RunOutcome};
use autra_core::{EffectKind, EffectRecord, EffectStatus, ModelOutput, ToolCallRequest, ToolSpec};
use autra_harness::{EchoTool, FakeModel, RunCommandTool, Tool, ToolError, WriteFileTool};

#[derive(Clone)]
struct RecordingApprover {
    decision: ApprovalDecision,
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
}

impl RecordingApprover {
    fn new(decision: ApprovalDecision) -> Self {
        Self {
            decision,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn seen(&self) -> Vec<ApprovalRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Approver for RecordingApprover {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.seen.lock().unwrap().push(request.clone());
        self.decision
    }
}

struct UnknownTool;

impl UnknownTool {
    fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for UnknownTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "unknown_effect".to_owned(),
            description: "A tool with an unclassified effect.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        Ok("ran".to_owned())
    }
}

fn call(call_id: &str, name: &str, arguments: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn model_with_tools(calls: Vec<ToolCallRequest>) -> FakeModel {
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

fn tool_effect(outcome: &RunOutcome) -> &EffectRecord {
    outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == EffectKind::ToolCall)
        .unwrap()
}

#[tokio::test]
async fn deny_policy_blocks_a_tool_without_asking() {
    let approver = RecordingApprover::new(ApprovalDecision::Approved);
    let agent = Agent::builder("policy")
        .model(model_with_tools(vec![call(
            "call-1",
            "echo",
            r#"{"text":"hi"}"#,
        )]))
        .tool(EchoTool)
        .policy(RulePolicy::new("test-v1").deny_tool("echo"))
        .approver(approver.clone())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let effect = tool_effect(&outcome);
    assert_eq!(effect.status, EffectStatus::Rejected);
    assert!(
        effect
            .error
            .as_deref()
            .unwrap()
            .contains("denied by policy `test-v1`")
    );
    assert!(approver.seen().is_empty());
}

#[tokio::test]
async fn unknown_effect_tools_require_approval() {
    let tool = UnknownTool::new();
    let agent = Agent::builder("policy")
        .model(model_with_tools(vec![call(
            "call-1",
            "unknown_effect",
            "{}",
        )]))
        .tool(tool)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let effect = tool_effect(&outcome);
    assert_eq!(effect.status, EffectStatus::Rejected);
    assert!(
        effect
            .error
            .as_deref()
            .unwrap()
            .contains("approval rejected")
    );
}

#[tokio::test]
async fn approval_carries_actor_revision_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = autra_harness::LocalWorkspace::open(dir.path()).unwrap();
    let approver = RecordingApprover::new(ApprovalDecision::Approved);
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let agent = Agent::builder("policy")
        .model(model_with_tools(vec![call(
            "call-1",
            "write_file",
            r#"{"path":"note.txt","content":"hello"}"#,
        )]))
        .tool(WriteFileTool::new(workspace))
        .policy(RulePolicy::new("test-v1"))
        .actor("operator")
        .approval_ttl(Duration::from_secs(60))
        .approver(approver.clone())
        .build()
        .unwrap();

    agent.run("go").await.unwrap();

    let seen = approver.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].policy_revision, "test-v1");
    assert_eq!(seen[0].actor.as_deref(), Some("operator"));
    assert!(seen[0].expires_at_unix_ms.unwrap() > before);
    assert!(
        seen[0]
            .preview
            .as_deref()
            .unwrap()
            .contains("create note.txt")
    );
}

#[tokio::test]
async fn deny_command_blocks_run_command_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = autra_harness::LocalWorkspace::open(dir.path()).unwrap();
    let approver = RecordingApprover::new(ApprovalDecision::Approved);
    let agent = Agent::builder("policy")
        .model(model_with_tools(vec![call(
            "call-1",
            "run_command",
            r#"{"command":"rm -rf /tmp/autra-never"}"#,
        )]))
        .tool(RunCommandTool::new(workspace))
        .policy(RulePolicy::new("test-v1").deny_command("rm -rf"))
        .approver(approver.clone())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let effect = tool_effect(&outcome);
    assert_eq!(effect.status, EffectStatus::Rejected);
    assert!(
        effect
            .error
            .as_deref()
            .unwrap()
            .contains("denied by policy")
    );
    assert!(approver.seen().is_empty());
}

#[tokio::test]
async fn read_only_tools_stay_allowed_without_a_policy() {
    let agent = Agent::builder("policy")
        .model(model_with_tools(vec![call(
            "call-1",
            "echo",
            r#"{"text":"hi"}"#,
        )]))
        .tool(EchoTool)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let effect = tool_effect(&outcome);
    assert_eq!(effect.status, EffectStatus::Committed);
}

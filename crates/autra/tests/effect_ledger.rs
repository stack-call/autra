use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use autra::{Agent, ApprovalDecision, ApprovalRequest, Approver, RunOptions};
use autra_core::{AttemptStatus, EffectKind, EffectStatus, ModelOutput, ToolCallRequest, ToolSpec};
use autra_harness::{
    EchoModel, EchoTool, FakeModel, LocalWorkspace, Tool, ToolError, WriteFileTool,
};
use tempfile::tempdir;

struct RejectingApprover {
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
}

#[async_trait]
impl Approver for RejectingApprover {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision {
        self.seen
            .lock()
            .expect("approval log poisoned")
            .push(request.clone());
        ApprovalDecision::Rejected
    }
}

struct SlowTool;

#[async_trait]
impl Tool for SlowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "slow".to_owned(),
            description: "Slow tool".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> autra_core::EffectClass {
        autra_core::EffectClass::ReadOnly
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok("late".to_owned())
    }
}

fn call(name: &str, arguments: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn scripted(calls: Vec<ToolCallRequest>) -> FakeModel {
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

#[tokio::test]
async fn records_a_committed_model_effect() {
    let agent = Agent::builder("effects").model(EchoModel).build().unwrap();

    let outcome = agent.run("hello").await.unwrap();

    let effects = outcome.effects();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].kind, EffectKind::ModelCall);
    assert_eq!(effects[0].status, EffectStatus::Committed);
    assert_eq!(effects[0].attempts.len(), 1);
    assert_eq!(effects[0].attempts[0].status, AttemptStatus::Succeeded);
    assert_eq!(effects[0].result.as_deref(), Some("echo: hello"));
    assert!(effects[0].id.as_str().starts_with("effect-"));
    assert_eq!(effects[0].intent_digest.len(), 64);
}

#[tokio::test]
async fn records_a_committed_tool_effect() {
    let agent = Agent::builder("effects")
        .model(scripted(vec![call("echo", r#"{"text":"hi"}"#)]))
        .tool(EchoTool)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Committed);
    assert_eq!(tool.result.as_deref(), Some("hi"));
    assert_eq!(tool.attempts[0].status, AttemptStatus::Succeeded);
    assert_eq!(outcome.effects().len(), 3);
}

#[tokio::test]
async fn records_an_unknown_tool_as_failed() {
    let agent = Agent::builder("effects")
        .model(scripted(vec![call("missing", "{}")]))
        .tool(EchoTool)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Failed);
    assert_eq!(tool.attempts[0].status, AttemptStatus::Failed);
    assert!(tool.error.as_deref().unwrap().contains("unknown tool"));
}

#[tokio::test]
async fn approval_rejection_is_bound_to_the_effect_record() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = scripted(vec![call(
        "write_file",
        r#"{"path":"note.txt","content":"hello"}"#,
    )]);
    let agent = Agent::builder("effects")
        .model(model)
        .tool(WriteFileTool::new(workspace))
        .approver(RejectingApprover { seen: seen.clone() })
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Rejected);
    assert!(tool.attempts.is_empty());
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].effect_id, tool.id);
}

#[tokio::test]
async fn tool_timeout_is_outcome_unknown() {
    let model = scripted(vec![call("slow", "{}")]);
    let agent = Agent::builder("effects")
        .model(model)
        .tool(SlowTool)
        .build()
        .unwrap();
    let options = RunOptions {
        effect_timeout: Some(Duration::from_millis(10)),
        ..RunOptions::default()
    };

    let outcome = agent.run_with_options("go", options, |_| {}).await.unwrap();

    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::OutcomeUnknown);
    assert_eq!(tool.attempts[0].status, AttemptStatus::OutcomeUnknown);
}

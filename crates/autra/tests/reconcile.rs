use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use autra::{
    Agent, ApprovalDecision, ApprovalRecord, ApprovalRequest, ApprovalStore, EffectJournal,
    MemoryJournal,
};
use autra_core::{
    EffectClass, EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus, HistoryItem,
    ModelCallSpec, ModelOutput, RunId, ToolCallRequest, ToolCallSpec, ToolSpec,
};
use autra_harness::{
    EchoModel, EchoTool, FakeModel, LocalWorkspace, Tool, ToolError, WriteFileTool,
};
use tempfile::tempdir;

struct NonIdempotentTool {
    calls: Arc<Mutex<usize>>,
}

impl NonIdempotentTool {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(0)),
        }
    }
}

#[async_trait]
impl Tool for NonIdempotentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "sensitive".to_owned(),
            description: "A non-idempotent test tool.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::NonIdempotent
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        *self.calls.lock().unwrap() += 1;
        Ok("ran".to_owned())
    }
}

fn tool_effect(run_id: &RunId, call: &ToolCallRequest, class: EffectKind) -> EffectRecord {
    let intent = EffectIntent::CallTool(ToolCallSpec { call: call.clone() });
    let mut effect = EffectRecord::new(
        EffectId::fresh(),
        class,
        Some(call.name.clone()),
        Some(call.call_id.clone()),
        "digest".to_owned(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    effect.set_status(EffectStatus::OutcomeUnknown);
    let _ = run_id;
    effect
}

fn model_effect() -> EffectRecord {
    let intent = EffectIntent::CallModel(ModelCallSpec {
        system: None,
        messages: vec![HistoryItem::User("go".to_owned())],
        tools: Vec::new(),
    });
    let mut effect = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ModelCall,
        None,
        None,
        "digest".to_owned(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    effect.set_status(EffectStatus::OutcomeUnknown);
    effect
}

fn call(name: &str, arguments: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

#[tokio::test]
async fn a_read_only_tool_is_retried() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-reconcile");
    let effect = tool_effect(
        &run_id,
        &call("echo", r#"{"text":"hi"}"#),
        EffectKind::ToolCall,
    );
    journal.record(&run_id, &effect).unwrap();
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .tool(EchoTool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert_eq!(report.retried, vec![effect.id.clone()]);
    assert!(report.unresolved.is_empty());
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::Committed);
    assert_eq!(stored[0].result.as_deref(), Some("hi"));
    assert_eq!(stored[0].reconciliations.len(), 1);
}

#[tokio::test]
async fn a_non_idempotent_tool_stays_unresolved() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-sensitive");
    let effect = tool_effect(&run_id, &call("sensitive", "{}"), EffectKind::ToolCall);
    journal.record(&run_id, &effect).unwrap();
    let tool = NonIdempotentTool::new();
    let calls = tool.calls.clone();
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .tool(tool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert!(report.retried.is_empty());
    assert_eq!(report.unresolved.len(), 1);
    assert!(report.unresolved[0].reason.contains("not safe"));
    assert_eq!(*calls.lock().unwrap(), 0);
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::OutcomeUnknown);
}

#[tokio::test]
async fn a_model_effect_is_reissued() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-model");
    let effect = model_effect();
    journal.record(&run_id, &effect).unwrap();
    let agent = Agent::builder("reconcile")
        .model(FakeModel::new(vec![ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }]))
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert_eq!(report.retried, vec![effect.id.clone()]);
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::Committed);
    assert_eq!(stored[0].result.as_deref(), Some("done"));
}

#[tokio::test]
async fn models_stay_unresolved_when_retry_is_disabled() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-model-only-tools");
    let effect = model_effect();
    journal.record(&run_id, &effect).unwrap();
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.reconcile_with(&run_id, false).await.unwrap();

    assert!(report.retried.is_empty());
    assert!(report.unresolved[0].reason.contains("requires a model"));
}

#[tokio::test]
async fn an_idempotent_write_requires_a_valid_approval() {
    let journal = MemoryJournal::new();
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let run_id = RunId::new("run-write");
    let call = call("write_file", r#"{"path":"note.txt","content":"hello"}"#);
    let mut effect = tool_effect(&run_id, &call, EffectKind::ToolCall);
    effect.requirements = WriteFileTool::new(workspace.clone()).execution_requirements();
    journal.record(&run_id, &effect).unwrap();
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .tool(WriteFileTool::new(workspace))
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert!(report.retried.is_empty());
    assert!(report.unresolved[0].reason.contains("approval"));
    assert!(!dir.path().join("note.txt").exists());

    let request = ApprovalRequest::new(&run_id, &effect.id, &call).with_policy(
        autra::LOCAL_POLICY_REVISION,
        None,
        Some(Duration::from_secs(300)),
    );
    journal
        .save_approval(&ApprovalRecord::from_request(
            &request,
            ApprovalDecision::Approved,
            autra::now_unix_ms(),
        ))
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert_eq!(report.retried, vec![effect.id.clone()]);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn reconciliation_requires_a_journal() {
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .build()
        .unwrap();

    let error = agent.reconcile(&RunId::new("run")).await.unwrap_err();

    assert!(matches!(error, autra::RunError::Protocol(_)));
}

#[tokio::test]
async fn a_deny_policy_prevents_reconciliation() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-reconcile-policy");
    let effect = tool_effect(
        &run_id,
        &call("echo", r#"{"text":"hi"}"#),
        EffectKind::ToolCall,
    );
    journal.record(&run_id, &effect).unwrap();
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .tool(EchoTool)
        .journal(journal.clone())
        .policy(autra::RulePolicy::new("test-v1").deny_tool("echo"))
        .build()
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert!(report.retried.is_empty());
    assert!(
        report.unresolved[0]
            .reason
            .contains("denied by current policy")
    );
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::OutcomeUnknown);
}

struct ReconcileableTool;

#[async_trait]
impl Tool for ReconcileableTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "reconcileable".to_owned(),
            description: "Needs a tool-specific reconciliation check.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Reconcileable
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        Ok("ran".to_owned())
    }
}

#[tokio::test]
async fn a_reconcileable_tool_needs_an_explicit_check() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-reconcileable");
    let effect = tool_effect(&run_id, &call("reconcileable", "{}"), EffectKind::ToolCall);
    journal.record(&run_id, &effect).unwrap();
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .tool(ReconcileableTool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert!(report.retried.is_empty());
    assert!(report.unresolved[0].reason.contains("Reconcileable"));
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::OutcomeUnknown);
}

#[tokio::test]
async fn an_approval_from_another_policy_revision_is_not_reused() {
    let journal = MemoryJournal::new();
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let run_id = RunId::new("run-stale-approval");
    let call = call("write_file", r#"{"path":"note.txt","content":"hello"}"#);
    let effect = tool_effect(&run_id, &call, EffectKind::ToolCall);
    journal.record(&run_id, &effect).unwrap();
    let request = ApprovalRequest::new(&run_id, &effect.id, &call).with_policy(
        "old-policy",
        None,
        Some(Duration::from_secs(300)),
    );
    journal
        .save_approval(&ApprovalRecord::from_request(
            &request,
            ApprovalDecision::Approved,
            autra::now_unix_ms(),
        ))
        .unwrap();
    let agent = Agent::builder("reconcile")
        .model(EchoModel)
        .tool(WriteFileTool::new(workspace))
        .journal(journal.clone())
        .policy(autra::RulePolicy::new("test-v1"))
        .build()
        .unwrap();

    let report = agent.reconcile(&run_id).await.unwrap();

    assert!(report.retried.is_empty());
    assert!(report.unresolved[0].reason.contains("approval"));
    assert!(!dir.path().join("note.txt").exists());
}

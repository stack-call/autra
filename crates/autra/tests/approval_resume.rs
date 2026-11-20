use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use autra::{
    Agent, ApprovalDecision, ApprovalRecord, ApprovalRequest, ApprovalStore, Approver,
    EffectJournal, MemoryJournal, RulePolicy,
};
use autra_core::{
    AgentPhase, AgentState, BudgetSnapshot, EffectId, EffectIntent, EffectKind, EffectRecord,
    EffectStatus, HistoryItem, ModelOutput, RunId, ToolCallRequest, ToolCallSpec,
};
use autra_harness::{FakeModel, LocalWorkspace, WriteFileTool, react_engine_state};
use tempfile::tempdir;

#[derive(Clone)]
struct RecordingApprover {
    decision: ApprovalDecision,
    seen: Arc<Mutex<usize>>,
}

impl RecordingApprover {
    fn new(decision: ApprovalDecision) -> Self {
        Self {
            decision,
            seen: Arc::new(Mutex::new(0)),
        }
    }

    fn seen(&self) -> usize {
        *self.seen.lock().unwrap()
    }
}

#[async_trait]
impl Approver for RecordingApprover {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        *self.seen.lock().unwrap() += 1;
        self.decision
    }
}

fn call() -> ToolCallRequest {
    ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        arguments: r#"{"path":"note.txt","content":"hello"}"#.to_owned(),
    }
}

fn checkpoint(journal: &MemoryJournal, run_id: &RunId, call: &ToolCallRequest) -> EffectRecord {
    let mut state = AgentState::new(run_id.clone(), BudgetSnapshot::new(8, 8));
    state.phase = AgentPhase::AwaitingTool;
    state.engine_state = react_engine_state(
        vec![
            HistoryItem::User("go".to_owned()),
            HistoryItem::Assistant {
                text: None,
                tool_calls: vec![call.clone()],
            },
        ],
        vec![call.clone()],
    );
    state.budget.turns_used = 1;
    state.budget.tool_calls_used = 1;
    journal.save_run(&state).unwrap();
    let intent = EffectIntent::CallTool(ToolCallSpec { call: call.clone() });
    let mut effect = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ToolCall,
        Some(call.name.clone()),
        Some(call.call_id.clone()),
        "digest".to_owned(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    effect.set_status(EffectStatus::AwaitingApproval);
    journal.record(run_id, &effect).unwrap();
    effect
}

fn save_decision(
    journal: &MemoryJournal,
    run_id: &RunId,
    effect: &EffectRecord,
    call: &ToolCallRequest,
    decision: ApprovalDecision,
    revision: &str,
    ttl: Option<Duration>,
) {
    let request = ApprovalRequest::new(run_id, &effect.id, call).with_policy(revision, None, ttl);
    let record = ApprovalRecord::from_request(&request, decision, autra::now_unix_ms());
    journal.save_approval(&record).unwrap();
}

fn agent(journal: &MemoryJournal, workspace: LocalWorkspace, approver: RecordingApprover) -> Agent {
    Agent::builder("resume")
        .model(FakeModel::new(vec![ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }]))
        .tool(WriteFileTool::new(workspace))
        .journal(journal.clone())
        .policy(RulePolicy::new("test-v1"))
        .approver(approver)
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_normal_run_persists_its_approval_decision() {
    let journal = MemoryJournal::new();
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let model = FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: vec![call()],
        },
        ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = Agent::builder("persist")
        .model(model)
        .tool(WriteFileTool::new(workspace))
        .journal(journal.clone())
        .policy(RulePolicy::new("test-v1"))
        .approver(RecordingApprover::new(ApprovalDecision::Approved))
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let tool = outcome
        .effects()
        .iter()
        .find(|effect| effect.kind == EffectKind::ToolCall)
        .unwrap();
    let record = journal.load_approval(&tool.id).unwrap().unwrap();
    assert_eq!(record.decision, ApprovalDecision::Approved);
    assert_eq!(record.policy_revision, "test-v1");
    assert_eq!(
        record.argument_digest,
        autra::digest_arguments("write_file", r#"{"path":"note.txt","content":"hello"}"#)
    );
}

#[tokio::test]
async fn resume_reuses_a_persisted_approval_without_asking() {
    let journal = MemoryJournal::new();
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let run_id = RunId::new("run-approval");
    let call = call();
    let effect = checkpoint(&journal, &run_id, &call);
    save_decision(
        &journal,
        &run_id,
        &effect,
        &call,
        ApprovalDecision::Approved,
        "test-v1",
        Some(Duration::from_secs(300)),
    );
    let approver = RecordingApprover::new(ApprovalDecision::Rejected);
    let agent = agent(&journal, workspace, approver.clone());

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(approver.seen(), 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn resume_re_asks_when_the_approval_expired() {
    let journal = MemoryJournal::new();
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let run_id = RunId::new("run-expired");
    let call = call();
    let effect = checkpoint(&journal, &run_id, &call);
    save_decision(
        &journal,
        &run_id,
        &effect,
        &call,
        ApprovalDecision::Approved,
        "test-v1",
        Some(Duration::ZERO),
    );
    let approver = RecordingApprover::new(ApprovalDecision::Approved);
    let agent = agent(&journal, workspace, approver.clone());

    agent.resume(&run_id).await.unwrap();

    assert_eq!(approver.seen(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn resume_honours_a_persisted_rejection() {
    let journal = MemoryJournal::new();
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let run_id = RunId::new("run-rejected");
    let call = call();
    let effect = checkpoint(&journal, &run_id, &call);
    save_decision(
        &journal,
        &run_id,
        &effect,
        &call,
        ApprovalDecision::Rejected,
        "test-v1",
        Some(Duration::from_secs(300)),
    );
    let approver = RecordingApprover::new(ApprovalDecision::Approved);
    let agent = agent(&journal, workspace, approver.clone());

    let outcome = agent.resume(&run_id).await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(approver.seen(), 0);
    assert!(!dir.path().join("note.txt").exists());
    let stored = journal.load(&run_id).unwrap();
    let tool = stored
        .iter()
        .find(|effect| effect.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.status, EffectStatus::Rejected);
}

#[tokio::test]
async fn resume_re_asks_when_the_policy_revision_changed() {
    let journal = MemoryJournal::new();
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let run_id = RunId::new("run-policy");
    let call = call();
    let effect = checkpoint(&journal, &run_id, &call);
    save_decision(
        &journal,
        &run_id,
        &effect,
        &call,
        ApprovalDecision::Approved,
        "old-policy",
        Some(Duration::from_secs(300)),
    );
    let approver = RecordingApprover::new(ApprovalDecision::Approved);
    let agent = agent(&journal, workspace, approver.clone());

    agent.resume(&run_id).await.unwrap();

    assert_eq!(approver.seen(), 1);
}

use async_trait::async_trait;
use autra_core::{
    AcceptedEvent, AgentState, AttemptStatus, EffectId, EffectKind, EffectRecord, EffectStatus,
    ModelCallSpec, ModelOutput, RunId, ToolCallRequest,
};
use autra_harness::{
    EchoTool, FakeModel, LocalWorkspace, ModelError, ModelProvider, WriteFileTool,
};
use autra_runtime::{
    Agent, ApprovalDecision, ApprovalRequest, Approver, EffectJournal, JournalError, MemoryJournal,
    OutboxEntry, RunError,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

#[derive(Clone, Default)]
struct SpyJournal {
    inner: MemoryJournal,
    enqueued: Arc<AtomicUsize>,
    claimed: Arc<AtomicUsize>,
    last_run: Arc<Mutex<Option<RunId>>>,
}

impl SpyJournal {
    fn enqueued(&self) -> usize {
        self.enqueued.load(Ordering::SeqCst)
    }

    fn claimed(&self) -> usize {
        self.claimed.load(Ordering::SeqCst)
    }

    fn pending(&self) -> usize {
        self.inner.claim_due_effects(1000, "check").unwrap().len()
    }

    fn last_run(&self) -> Option<RunId> {
        self.last_run.lock().unwrap().clone()
    }
}

impl EffectJournal for SpyJournal {
    fn record(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        *self.last_run.lock().unwrap() = Some(run_id.clone());
        self.inner.record(run_id, effect)
    }

    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError> {
        self.inner.load(run_id)
    }

    fn save_run(&self, state: &AgentState) -> Result<(), JournalError> {
        self.inner.save_run(state)
    }

    fn load_run(&self, run_id: &RunId) -> Result<Option<AgentState>, JournalError> {
        self.inner.load_run(run_id)
    }

    fn commit_event(
        &self,
        run_id: &RunId,
        event: &AcceptedEvent,
        state: &AgentState,
        effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        self.inner.commit_event(run_id, event, state, effects)
    }

    fn load_events(&self, run_id: &RunId) -> Result<Vec<AcceptedEvent>, JournalError> {
        self.inner.load_events(run_id)
    }

    fn enqueue_effect(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        self.enqueued.fetch_add(1, Ordering::SeqCst);
        self.inner.enqueue_effect(run_id, effect)
    }

    fn claim_due_effects(
        &self,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let rows = self.inner.claim_due_effects(limit, claim_id)?;
        self.claimed.fetch_add(rows.len(), Ordering::SeqCst);
        Ok(rows)
    }

    fn claim_due_effects_for_run(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let rows = self
            .inner
            .claim_due_effects_for_run(run_id, limit, claim_id)?;
        self.claimed.fetch_add(rows.len(), Ordering::SeqCst);
        Ok(rows)
    }

    fn complete_outbox(&self, effect_id: &EffectId) -> Result<(), JournalError> {
        self.inner.complete_outbox(effect_id)
    }

    fn release_outbox(&self, effect_id: &EffectId) -> Result<(), JournalError> {
        self.inner.release_outbox(effect_id)
    }
}

struct FailingModel;

#[async_trait]
impl ModelProvider for FailingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Err(ModelError::new("model-boom"))
    }
}

struct RejectingApprover;

#[async_trait]
impl Approver for RejectingApprover {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Rejected
    }
}

fn call(call_id: &str, name: &str, arguments: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn tool_calls(calls: Vec<ToolCallRequest>) -> FakeModel {
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

fn tool_effects(effects: &[EffectRecord]) -> Vec<&EffectRecord> {
    effects
        .iter()
        .filter(|effect| effect.kind == EffectKind::ToolCall)
        .collect()
}

#[tokio::test]
async fn a_tool_run_claims_each_effect_from_the_outbox() {
    let journal = SpyJournal::default();
    let agent = Agent::builder("outbox")
        .model(tool_calls(vec![call("call-1", "echo", r#"{"text":"hi"}"#)]))
        .tool(EchoTool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let effects = outcome.effects();
    let tools = tool_effects(effects);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].status, EffectStatus::Committed);
    assert_eq!(tools[0].result.as_deref(), Some("hi"));
    let run_id = outcome.state().run_id.clone();
    assert_eq!(journal.load(&run_id).unwrap(), effects);
    assert_eq!(journal.enqueued(), 3);
    assert_eq!(journal.claimed(), 3);
    assert_eq!(journal.pending(), 0);
}

#[tokio::test]
async fn a_parallel_tool_batch_is_claimed_and_drained() {
    let journal = SpyJournal::default();
    let agent = Agent::builder("outbox")
        .model(tool_calls(vec![
            call("call-1", "echo", r#"{"text":"a"}"#),
            call("call-2", "echo", r#"{"text":"b"}"#),
        ]))
        .tool(EchoTool)
        .max_parallel_tool_calls(2)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let effects = outcome.effects();
    let tools = tool_effects(effects);
    assert_eq!(tools.len(), 2);
    assert!(
        tools
            .iter()
            .all(|effect| effect.status == EffectStatus::Committed)
    );
    let run_id = outcome.state().run_id.clone();
    assert_eq!(journal.load(&run_id).unwrap(), effects);
    assert_eq!(journal.enqueued(), 4);
    assert_eq!(journal.claimed(), 4);
    assert_eq!(journal.pending(), 0);
}

#[tokio::test]
async fn a_model_failure_fails_the_claimed_effect() {
    let journal = SpyJournal::default();
    let agent = Agent::builder("outbox")
        .model(FailingModel)
        .journal(journal.clone())
        .build()
        .unwrap();

    let error = agent.run("go").await.unwrap_err();
    assert!(matches!(error, RunError::Model(_)));

    let run_id = journal.last_run().expect("the run id was journaled");
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, EffectStatus::Failed);
    assert_eq!(stored[0].attempts[0].status, AttemptStatus::Failed);
    assert!(stored[0].error.as_deref().unwrap().contains("model-boom"));
    assert_eq!(journal.enqueued(), 1);
    assert_eq!(journal.claimed(), 1);
    assert_eq!(journal.pending(), 0);
}

#[tokio::test]
async fn a_rejected_tool_never_enters_the_outbox() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();
    let journal = SpyJournal::default();
    let agent = Agent::builder("outbox")
        .model(tool_calls(vec![call(
            "call-1",
            "write_file",
            r#"{"path":"note.txt","content":"hello"}"#,
        )]))
        .tool(WriteFileTool::new(workspace))
        .approver(RejectingApprover)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let effects = outcome.effects();
    let tools = tool_effects(effects);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].status, EffectStatus::Rejected);
    assert_eq!(journal.load(&outcome.state().run_id).unwrap(), effects);
    // Two model calls enter the outbox; the rejected tool does not.
    assert_eq!(journal.enqueued(), 2);
    assert_eq!(journal.claimed(), 2);
    assert_eq!(journal.pending(), 0);
}

#[tokio::test]
async fn an_unknown_tool_is_reported_failed_through_the_outbox() {
    let journal = SpyJournal::default();
    let agent = Agent::builder("outbox")
        .model(tool_calls(vec![call("call-1", "missing", "{}")]))
        .tool(EchoTool)
        .journal(journal.clone())
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    let effects = outcome.effects();
    let tools = tool_effects(effects);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].status, EffectStatus::Failed);
    assert!(tools[0].error.as_deref().unwrap().contains("unknown tool"));
    assert_eq!(journal.load(&outcome.state().run_id).unwrap(), effects);
    assert_eq!(journal.enqueued(), 3);
    assert_eq!(journal.claimed(), 3);
    assert_eq!(journal.pending(), 0);
}

#[tokio::test]
async fn a_claimed_tool_emits_started_before_its_result() {
    use autra_runtime::RunEvent;

    let journal = SpyJournal::default();
    let agent = Agent::builder("outbox")
        .model(tool_calls(vec![call("call-1", "echo", r#"{"text":"hi"}"#)]))
        .tool(EchoTool)
        .journal(journal.clone())
        .build()
        .unwrap();
    let mut events = Vec::new();

    agent
        .run_with_options("go", Default::default(), |event| events.push(event.clone()))
        .await
        .unwrap();

    let started = events
        .iter()
        .position(|event| matches!(event, RunEvent::ToolCallStarted { call_id, .. } if call_id == "call-1"))
        .expect("started event");
    let finished = events
        .iter()
        .position(
            |event| matches!(event, RunEvent::ToolResult { call_id, .. } if call_id == "call-1"),
        )
        .expect("result event");
    assert!(started < finished);
}

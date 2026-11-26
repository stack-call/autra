use async_trait::async_trait;
use autra::{
    Agent, CommitAuthority, DispatchOptions, DurableTransitionStore, EffectJournal, EffectOutcome,
    EffectResult, GlobalScheduler, LocalDispatcher, LocalEngine, MemoryJournal, ModelError,
    ModelProvider, ResultIdentity, SchedulerLimits, ToolRegistry,
};
use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, BudgetSnapshot, EffectId, EffectIntent, EffectKind,
    EffectRecord, EffectStatus, EventKind, ModelCallSpec, ModelOutput, RunId, ToolCallRequest,
    ToolCallSpec,
};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_harness::{EchoTool, FakeModel};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
fn output() -> ModelOutput {
    ModelOutput {
        text: Some("thinking".into()),
        tool_calls: vec![ToolCallRequest {
            call_id: "call-1".into(),
            name: "echo".into(),
            arguments: r#"{"text":"hi"}"#.into(),
        }],
        usage: Some(autra_core::TokenUsage {
            prompt_tokens: 3,
            completion_tokens: 2,
            total_tokens: 5,
        }),
    }
}
fn request() -> ModelCallSpec {
    ModelCallSpec {
        system: None,
        messages: vec![],
        tools: vec![],
    }
}
fn options() -> DispatchOptions<'static> {
    DispatchOptions {
        limit: 8,
        max_parallel: 2,
        timeout: None,
        cancel: None,
        lease: Duration::from_secs(60),
        budget: None,
        child_execution: None,
        delta: None,
        scheduler: None,
        result_inbox: true,
    }
}
fn stage(
    j: &dyn DurableTransitionStore,
    intent: EffectIntent,
) -> (RunId, EffectRecord, AgentState) {
    let run = RunId::fresh();
    let kind = match &intent {
        EffectIntent::CallModel(_) => EffectKind::ModelCall,
        _ => EffectKind::ToolCall,
    };
    let call = intent.tool_call();
    let mut r = EffectRecord::new(
        EffectId::fresh(),
        kind,
        call.map(|c| c.name.clone()),
        call.map(|c| c.call_id.clone()),
        "digest".into(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    r.status = EffectStatus::Authorized;
    let mut state = AgentState::new(run.clone(), BudgetSnapshot::new(8, 8));
    state.state_version = 1;
    state.phase = if kind == EffectKind::ModelCall {
        AgentPhase::AwaitingModel
    } else {
        AgentPhase::AwaitingTool
    };
    j.commit_transition(
        &run,
        0,
        &AcceptedEvent {
            seq: 1,
            kind: EventKind::RunStarted { input: "go".into() },
        },
        &state,
        std::slice::from_ref(&r),
    )
    .unwrap();
    (run, r, state)
}
fn engine(j: Arc<dyn DurableTransitionStore>, model: impl ModelProvider + 'static) -> LocalEngine {
    let mut tools = ToolRegistry::new();
    tools.register(EchoTool);
    LocalEngine::new(
        Arc::new(LocalDispatcher::new(Arc::new(model), tools.clone())),
        tools,
    )
    .with_journal(j)
}
fn journals(path: &std::path::Path) -> Vec<Arc<dyn DurableTransitionStore>> {
    vec![
        Arc::new(MemoryJournal::new()),
        Arc::new(SqliteEffectJournal::open(path).unwrap()),
    ]
}
#[tokio::test]
async fn publication_consumption_rollback_deduplication_and_fencing_on_both_journals() {
    let dir = tempfile::tempdir().unwrap();
    for j in journals(&dir.path().join("inbox.db")) {
        let (run, r, state) = stage(j.as_ref(), EffectIntent::CallModel(request()));
        let executor = engine(j.clone(), FakeModel::new([output()])).effect_executor();
        executor.execute_pending(&run, options()).await.unwrap();
        let rs = j.pending_results(&run, 10).unwrap();
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].outcome, EffectOutcome::Model(output()));
        assert!(j.peek_due_effects(Some(&run), 10).unwrap().is_empty());
        let stored = j.load_effect(&run, &r.id).unwrap().unwrap();
        assert_eq!(stored.status, EffectStatus::Committed);
        j.record_result_claimed(&run, &stored, "original-claim-gone", &rs[0])
            .unwrap();
        let mut conflict = rs[0].clone();
        conflict.outcome = EffectOutcome::ModelFailed("different".into());
        assert!(
            j.record_result_claimed(&run, &stored, "stale", &conflict)
                .is_err()
        );
        j.claim_run(&run, "old", Duration::from_secs(60)).unwrap();
        j.release_run(&run, "old").unwrap();
        assert!(j.claim_run(&run, "new", Duration::from_secs(60)).unwrap());
        assert!(j.resolve_effect_owned(&run, &r, "old").is_err());
        assert!(j.peek_due_effects(Some(&run), 10).unwrap().is_empty());
        let mut next = state.clone();
        next.state_version = 2;
        next.phase = AgentPhase::Completed;
        let event = AcceptedEvent {
            seq: 2,
            kind: EventKind::ModelResult(output()),
        };
        let old = CommitAuthority {
            owner_id: "old".into(),
            result: Some(rs[0].identity.clone()),
        };
        assert!(
            j.commit_owned_transition(&run, 1, &event, &next, &[], None, &old)
                .is_err()
        );
        let missing = CommitAuthority {
            owner_id: "new".into(),
            result: Some(ResultIdentity {
                effect_id: r.id.clone(),
                attempt_id: "missing".into(),
            }),
        };
        assert!(
            j.commit_owned_transition(
                &run,
                1,
                &event,
                &next,
                std::slice::from_ref(&r),
                None,
                &missing
            )
            .is_err()
        );
        assert_eq!(j.load_run(&run).unwrap().unwrap(), state);
        assert_eq!(j.load_events(&run).unwrap().len(), 1);
        assert!(j.peek_due_effects(Some(&run), 10).unwrap().is_empty());
        assert_eq!(j.pending_results(&run, 10).unwrap().len(), 1);
        let owner = CommitAuthority {
            owner_id: "new".into(),
            result: Some(rs[0].identity.clone()),
        };
        j.commit_owned_transition(&run, 1, &event, &next, &[], None, &owner)
            .unwrap();
        assert!(j.pending_results(&run, 10).unwrap().is_empty());
        let mut duplicate = next.clone();
        duplicate.state_version = 3;
        assert!(
            j.commit_owned_transition(
                &run,
                2,
                &AcceptedEvent {
                    seq: 3,
                    kind: event.kind.clone()
                },
                &duplicate,
                &[],
                None,
                &owner
            )
            .is_err()
        );
        assert_eq!(j.load_run(&run).unwrap().unwrap(), next);
        assert_eq!(j.load_events(&run).unwrap().len(), 2);
    }
}
#[tokio::test]
async fn interrupted_tools_become_unknown_without_reexecution() {
    let dir = tempfile::tempdir().unwrap();
    for j in journals(&dir.path().join("unknown.db")) {
        let call = ToolCallRequest {
            call_id: "one".into(),
            name: "echo".into(),
            arguments: r#"{"text":"hi"}"#.into(),
        };
        let (run, mut r, _) = stage(j.as_ref(), EffectIntent::CallTool(ToolCallSpec { call }));
        j.claim_effect(&r.id, "crashed", Duration::from_secs(60))
            .unwrap()
            .unwrap();
        r.status = EffectStatus::Dispatched;
        j.record_claimed(&run, &r, "crashed").unwrap();
        j.release_outbox(&r.id, "crashed").unwrap();
        engine(j.clone(), FakeModel::default())
            .effect_executor()
            .execute_pending(&run, options())
            .await
            .unwrap();
        let result = j.pending_results(&run, 1).unwrap().remove(0);
        assert!(matches!(result.outcome,EffectOutcome::Tool(ref o) if o.outcome_unknown));
        assert_eq!(
            j.load_effect(&run, &r.id).unwrap().unwrap().status,
            EffectStatus::OutcomeUnknown
        );
        assert!(
            j.record_result_claimed(
                &run,
                &r,
                "crashed",
                &EffectResult {
                    identity: ResultIdentity {
                        effect_id: r.id.clone(),
                        attempt_id: "stale".into()
                    },
                    outcome: EffectOutcome::Cancelled,
                    budget: None
                }
            )
            .is_err()
        );
    }
}
#[tokio::test]
async fn expired_claims_cannot_write_or_renew_before_takeover() {
    let dir = tempfile::tempdir().unwrap();
    for j in journals(&dir.path().join("expired.db")) {
        let (run, mut r, _) = stage(j.as_ref(), EffectIntent::CallModel(request()));
        j.claim_due_effects_for_run_with_lease(&run, 1, "expired", Duration::ZERO)
            .unwrap();
        assert!(
            !j.renew_claim(&r.id, "expired", Duration::from_secs(60))
                .unwrap()
        );
        r.status = EffectStatus::Dispatched;
        assert!(
            j.record_claimed(&run, &r, "expired")
                .unwrap_err()
                .is_claim_lost()
        );
        assert!(
            j.record_result_claimed(
                &run,
                &r,
                "expired",
                &EffectResult {
                    identity: ResultIdentity {
                        effect_id: r.id.clone(),
                        attempt_id: "expired".into()
                    },
                    outcome: EffectOutcome::Cancelled,
                    budget: None
                }
            )
            .unwrap_err()
            .is_claim_lost()
        );
        assert_eq!(
            j.load_effect(&run, &r.id).unwrap().unwrap().status,
            EffectStatus::Authorized
        );
        assert!(j.pending_results(&run, 10).unwrap().is_empty());
    }
}
struct CountingModel(Arc<AtomicUsize>);
#[async_trait]
impl ModelProvider for CountingModel {
    async fn complete(&self, _: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(output())
    }
}
#[tokio::test]
async fn competing_executors_dispatch_once() {
    let dir = tempfile::tempdir().unwrap();
    for j in journals(&dir.path().join("competing.db")) {
        let (run, _, _) = stage(j.as_ref(), EffectIntent::CallModel(request()));
        let calls = Arc::new(AtomicUsize::new(0));
        let e = engine(j.clone(), CountingModel(calls.clone())).effect_executor();
        let (a, b) = tokio::join!(
            e.execute_pending(&run, options()),
            e.execute_pending(&run, options())
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(j.pending_results(&run, 10).unwrap().len(), 1);
    }
}
#[tokio::test]
async fn independent_sqlite_connections_compete_without_duplicate_execution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connections.db");
    let first = Arc::new(SqliteEffectJournal::open(&path).unwrap());
    let second = Arc::new(SqliteEffectJournal::open(&path).unwrap());
    let (run, _, _) = stage(first.as_ref(), EffectIntent::CallModel(request()));
    let calls = Arc::new(AtomicUsize::new(0));
    let a = engine(first.clone(), CountingModel(calls.clone())).effect_executor();
    let b = engine(second, CountingModel(calls.clone())).effect_executor();
    let (a, b) = tokio::join!(
        a.execute_pending(&run, options()),
        b.execute_pending(&run, options())
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.pending_results(&run, 10).unwrap().len(), 1);
}
#[tokio::test]
async fn explicit_retry_preserves_generations_and_supersedes_unaccepted_results() {
    let dir = tempfile::tempdir().unwrap();
    for j in journals(&dir.path().join("retry.db")) {
        let (run, r, _) = stage(j.as_ref(), EffectIntent::CallModel(request()));
        let e = engine(j.clone(), FakeModel::new([output(), output()])).effect_executor();
        e.execute_pending(&run, options()).await.unwrap();
        let first = j.pending_results(&run, 1).unwrap().remove(0);
        let mut r = j.load_effect(&run, &r.id).unwrap().unwrap();
        let generation = r.claim_generation();
        r.status = EffectStatus::Authorized;
        j.authorize_effect(&run, &r).unwrap();
        e.execute_pending(&run, options()).await.unwrap();
        assert!(
            j.load_effect(&run, &r.id)
                .unwrap()
                .unwrap()
                .claim_generation()
                > generation
        );
        let rs = j.pending_results(&run, 10).unwrap();
        assert_eq!(rs.len(), 1);
        assert_ne!(rs[0].identity, first.identity);
    }
}
struct BlockedModel {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl ModelProvider for BlockedModel {
    async fn complete(&self, _: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(output())
    }
}
#[tokio::test]
async fn worker_outlives_coordinator_and_resume_consumes_full_result_without_reissuing_model() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("handoff.db");
    let reader = SqliteEffectJournal::open(&path).unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let agent = Agent::builder("handoff")
        .model(BlockedModel {
            started: started.clone(),
            release: release.clone(),
        })
        .tool(EchoTool)
        .journal(SqliteEffectJournal::open(&path).unwrap())
        .pooled_execution(true)
        .build()
        .unwrap();
    let coordinator = tokio::spawn(async move { agent.run("go").await });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    let run = reader.list_run_ids().unwrap().remove(0);
    coordinator.abort();
    let _ = coordinator.await;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        while reader.pending_results(&run, 1).unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Intentionally omit pooled_execution: persisted Effects identify their protocol.
    let agent = Agent::builder("handoff")
        .model(FakeModel::new([ModelOutput {
            text: Some("done".into()),
            tool_calls: vec![],
            usage: None,
        }]))
        .tool(EchoTool)
        .journal(SqliteEffectJournal::open(&path).unwrap())
        .build()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), agent.resume(&run))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.answer(), Some("done"));
    assert_eq!(result.effects().len(), 3);
    assert!(reader.pending_results(&run, 10).unwrap().is_empty());
    let events = reader.load_events(&run).unwrap();
    assert!(events.windows(2).all(|p| p[0].seq < p[1].seq));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(&e.kind,EventKind::ModelResult(o) if !o.tool_calls.is_empty()))
            .count(),
        1
    );
}
#[tokio::test]
async fn waiting_for_capacity_does_not_take_an_effect_lease() {
    let j = Arc::new(MemoryJournal::new());
    let (run, r, _) = stage(j.as_ref(), EffectIntent::CallModel(request()));
    let scheduler = Arc::new(GlobalScheduler::new(SchedulerLimits::new(1, 1, 1)));
    let held = scheduler
        .acquire(autra::ResourceKind::Model, &RunId::fresh(), None)
        .await
        .unwrap();
    let e = engine(j.clone(), FakeModel::new([output()]))
        .with_scheduler(scheduler)
        .effect_executor();
    let task_run = run.clone();
    let task = tokio::spawn(async move { e.execute_pending(&task_run, options()).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        j.claim_effect(&r.id, "probe", Duration::from_secs(60))
            .unwrap()
            .is_some()
    );
    j.release_outbox(&r.id, "probe").unwrap();
    drop(held);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(j.pending_results(&run, 10).unwrap().len(), 1);
}
#[tokio::test]
async fn cancellation_retires_work_even_with_an_unavailable_environment() {
    let j = Arc::new(MemoryJournal::new());
    let (run, mut r, _) = stage(j.as_ref(), EffectIntent::CallModel(request()));
    r.requirements = autra_core::ExecutionRequirements {
        environment: Some(autra_core::EnvironmentAffinity::Required("gone".into())),
        file_api: true,
        command_execution: false,
    };
    j.record(&run, &r).unwrap();
    let cancel = autra_core::CancelFlag::new();
    cancel.cancel();
    engine(j.clone(), FakeModel::default())
        .effect_executor()
        .execute_pending(
            &run,
            DispatchOptions {
                cancel: Some(&cancel),
                ..options()
            },
        )
        .await
        .unwrap();
    assert!(j.peek_due_effects(Some(&run), 10).unwrap().is_empty());
    assert_eq!(
        j.load_effect(&run, &r.id).unwrap().unwrap().status,
        EffectStatus::CancelledBeforeDispatch
    );
}
#[tokio::test]
async fn batches_larger_than_resource_pools_complete_in_both_modes() {
    for pooled in [false, true] {
        let first = ModelOutput {
            text: None,
            usage: None,
            tool_calls: (0..3)
                .map(|i| ToolCallRequest {
                    call_id: format!("call-{i}"),
                    name: "echo".into(),
                    arguments: r#"{"text":"hi"}"#.into(),
                })
                .collect(),
        };
        let mut b = Agent::builder("bounded")
            .model(FakeModel::new([
                first,
                ModelOutput {
                    text: Some("done".into()),
                    usage: None,
                    tool_calls: vec![],
                },
            ]))
            .tool(EchoTool)
            .max_parallel_tool_calls(3)
            .scheduler_limits(SchedulerLimits::new(1, 1, 1))
            .pooled_execution(pooled);
        if pooled {
            b = b.journal(MemoryJournal::new());
        }
        let a = b.build().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), a.run("go"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.answer(), Some("done"));
        assert_eq!(result.effects().len(), 5);
    }
}
#[tokio::test]
async fn pooled_rejections_do_not_dispatch_or_hang() {
    let dir = tempfile::tempdir().unwrap();
    let ws = autra_harness::LocalWorkspace::open(dir.path()).unwrap();
    let first = ModelOutput {
        text: None,
        usage: None,
        tool_calls: vec![ToolCallRequest {
            call_id: "write".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"never.txt","content":"no"}"#.into(),
        }],
    };
    let a = Agent::builder("rejection")
        .model(FakeModel::new([
            first,
            ModelOutput {
                text: Some("done".into()),
                usage: None,
                tool_calls: vec![],
            },
        ]))
        .tool(autra_harness::WriteFileTool::new(ws))
        .journal(MemoryJournal::new())
        .pooled_execution(true)
        .build()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), a.run("go"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.answer(), Some("done"));
    assert!(!dir.path().join("never.txt").exists());
    assert!(
        result
            .effects()
            .iter()
            .any(|r| r.status == EffectStatus::Rejected)
    );
}

struct RoutedFile(&'static str);
#[async_trait]
impl autra::ExecutionEnvironment for RoutedFile {
    fn id(&self) -> &str {
        "resource:one"
    }
    fn capabilities(&self) -> autra::EnvironmentCapabilities {
        autra::EnvironmentCapabilities {
            file_api: true,
            ..Default::default()
        }
    }
    async fn read_file(
        &self,
        _: &str,
        _: usize,
    ) -> Result<autra::FileRead, autra::EnvironmentError> {
        Ok(autra::FileRead {
            bytes: self.0.as_bytes().to_vec(),
            truncated: false,
        })
    }
}
#[tokio::test]
async fn dispatcher_passes_the_selected_environment_to_the_tool() {
    let scheduler = Arc::new(GlobalScheduler::default());
    scheduler
        .register_environment(Arc::new(RoutedFile("selected")), 1)
        .unwrap();
    let first = ModelOutput {
        text: None,
        usage: None,
        tool_calls: vec![ToolCallRequest {
            call_id: "read".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"remote.txt"}"#.into(),
        }],
    };
    let a = Agent::builder("placement")
        .model(FakeModel::new([
            first,
            ModelOutput {
                text: Some("done".into()),
                usage: None,
                tool_calls: vec![],
            },
        ]))
        .tool(autra_harness::ReadFileTool::in_environment(Arc::new(
            RoutedFile("constructor"),
        )))
        .scheduler(scheduler)
        .journal(MemoryJournal::new())
        .pooled_execution(true)
        .build()
        .unwrap();
    let result = a.run("go").await.unwrap();
    let tool = result
        .effects()
        .iter()
        .find(|r| r.kind == EffectKind::ToolCall)
        .unwrap();
    assert_eq!(tool.result.as_deref(), Some("selected"));
    assert_eq!(
        tool.requirements.environment,
        Some(autra_core::EnvironmentAffinity::Required(
            "resource:one".into()
        ))
    );
}

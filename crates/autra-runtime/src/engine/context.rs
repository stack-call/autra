//! In-process Run context resolution. Workers borrow no Coordinator state.
//! Journal remains authoritative; registrations are live handles, not recovery data.
use super::EffectExecutor;
use super::pool::PoolEvent;
use autra_core::{BudgetHandle, CancelFlag, RunId};
use autra_execution::ChildExecutionContext;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{Notify, mpsc::Sender, watch};

pub(super) const RECOVERY_SCAN_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct RunExecutionContext {
    pub run_id: RunId,
    pub executor: EffectExecutor,
    pub cancel: CancelFlag,
    pub budget: BudgetHandle,
    pub events: Sender<PoolEvent>,
    pub work_ready: Arc<Notify>,
    pub result_ready: Arc<Notify>,
    pub timeout: Option<Duration>,
    pub max_parallel: usize,
    pub child_execution: ChildExecutionContext,
}
struct RegistryInner {
    contexts: Mutex<BTreeMap<RunId, Weak<RunExecutionContext>>>,
    sweep: watch::Sender<u64>,
    sweeper: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
#[derive(Clone)]
pub(super) struct RunContextRegistry {
    inner: Arc<RegistryInner>,
}
impl Default for RunContextRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                contexts: Mutex::new(BTreeMap::new()),
                sweep: watch::channel(0).0,
                sweeper: Mutex::new(None),
            }),
        }
    }
}
impl RunContextRegistry {
    pub fn register(
        &self,
        mut context: RunExecutionContext,
    ) -> Result<ContextRegistration, autra_execution::RunError> {
        let context = {
            let mut contexts = self.inner.contexts.lock().expect("context lock poisoned");
            if let Some(old) = contexts.get(&context.run_id).and_then(Weak::upgrade) {
                if !old.events.is_closed() {
                    return Err(autra_execution::RunError::Protocol(
                        "Run already has a live execution context".into(),
                    ));
                }
                if !old.budget.advance_to(&context.budget.snapshot()) {
                    return Err(autra_execution::RunError::Protocol(
                        "Run budget limits changed across context generations".into(),
                    ));
                }
                context.budget = old.budget.clone();
            }
            let context = Arc::new(context);
            contexts.insert(context.run_id.clone(), Arc::downgrade(&context));
            context
        };
        let mut sweeper = self.inner.sweeper.lock().expect("sweep lock poisoned");
        if sweeper.as_ref().is_none_or(|task| task.is_finished()) {
            let weak = Arc::downgrade(&self.inner);
            *sweeper = Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(RECOVERY_SCAN_INTERVAL).await;
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    inner
                        .contexts
                        .lock()
                        .expect("context lock poisoned")
                        .retain(|_, context| context.strong_count() != 0);
                    inner
                        .sweep
                        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
                }
            }));
        }
        drop(sweeper);
        Ok(ContextRegistration { context })
    }
    pub fn resolve(&self, run: &RunId) -> Option<Arc<RunExecutionContext>> {
        self.inner
            .contexts
            .lock()
            .expect("context lock poisoned")
            .get(run)
            .and_then(Weak::upgrade)
    }
    pub fn recovery_ticks(&self) -> watch::Receiver<u64> {
        self.inner.sweep.subscribe()
    }
}
// The Run owns the registration. A worker borrows a shared context handle,
// which keeps committed work and its budget alive if the Run detaches.
pub(super) struct ContextRegistration {
    pub context: Arc<RunExecutionContext>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalDispatcher, LocalEngine};
    use async_trait::async_trait;
    use autra_core::{BudgetSnapshot, ModelCallSpec, ModelOutput};
    use autra_execution::{ModelError, ModelProvider, RunEvent, ToolRegistry};
    struct Model;
    #[async_trait]
    impl ModelProvider for Model {
        async fn complete(&self, _: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
            Ok(ModelOutput {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
            })
        }
    }
    fn context(
        run: RunId,
        budget: u32,
    ) -> (RunExecutionContext, tokio::sync::mpsc::Receiver<PoolEvent>) {
        let tools = ToolRegistry::new();
        let engine = LocalEngine::new(
            Arc::new(LocalDispatcher::new(Arc::new(Model), tools.clone())),
            tools,
        );
        let (events, receiver) = tokio::sync::mpsc::channel(8);
        (
            RunExecutionContext {
                run_id: run,
                executor: engine.effect_executor(),
                cancel: CancelFlag::new(),
                budget: BudgetHandle::new(BudgetSnapshot::new(8, budget)),
                events,
                work_ready: Arc::new(Notify::new()),
                result_ready: Arc::new(Notify::new()),
                timeout: Some(Duration::from_secs(budget as u64)),
                max_parallel: budget as usize,
                child_execution: ChildExecutionContext::default(),
            },
            receiver,
        )
    }
    #[tokio::test]
    async fn run_contexts_isolate_cancellation_budget_options_and_stream_routing() {
        let registry = RunContextRegistry::default();
        let (a, mut a_events) = context(RunId::fresh(), 2);
        let (b, mut b_events) = context(RunId::fresh(), 8);
        let a = registry.register(a).unwrap();
        let b = registry.register(b).unwrap();
        let a = registry.resolve(&a.context.run_id).unwrap();
        let b = registry.resolve(&b.context.run_id).unwrap();
        a.cancel.cancel();
        assert!(!b.cancel.is_cancelled());
        assert!(a.budget.reserve_child(1, 2).is_some());
        assert!(a.budget.clone().reserve_child(1, 1).is_none());
        assert!(a.budget.advance_to(&BudgetSnapshot::new(8, 2)));
        assert_eq!(a.budget.remaining_tool_calls(), 0);
        assert_eq!(b.budget.remaining_tool_calls(), 8);
        assert_eq!(a.timeout, Some(Duration::from_secs(2)));
        assert_eq!(b.max_parallel, 8);
        a.events
            .try_send(PoolEvent::Stream(RunEvent::ModelDelta { text: "A".into() }))
            .unwrap();
        assert!(
            matches!(a_events.try_recv(),Ok(PoolEvent::Stream(RunEvent::ModelDelta{text})) if text=="A")
        );
        assert!(b_events.try_recv().is_err());
    }
    #[tokio::test]
    async fn stale_registration_cannot_remove_or_overwrite_its_replacement() {
        let registry = RunContextRegistry::default();
        let run = RunId::fresh();
        let (old, old_events) = context(run.clone(), 2);
        let old = registry.register(old).unwrap();
        let (duplicate, _receiver) = context(run.clone(), 8);
        assert!(registry.register(duplicate).is_err());
        assert!(old.context.budget.reserve_child(1, 1).is_some());
        drop(old_events);
        let (wrong, _wrong_events) = context(run.clone(), 8);
        assert!(registry.register(wrong).is_err());
        let (replacement, _new_events) = context(run.clone(), 2);
        let replacement = registry.register(replacement).unwrap();
        assert_eq!(replacement.context.budget.remaining_tool_calls(), 1);
        assert!(replacement.context.budget.reserve_child(1, 1).is_some());
        assert_eq!(old.context.budget.remaining_tool_calls(), 0);
        drop(old);
        assert!(Arc::ptr_eq(
            &registry.resolve(&run).unwrap(),
            &replacement.context
        ));
        drop(replacement);
        assert!(registry.resolve(&run).is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn registered_runs_share_one_recovery_clock() {
        let registry = RunContextRegistry::default();
        let mut first = registry.recovery_ticks();
        let mut second = registry.recovery_ticks();
        let (a, _a_events) = context(RunId::fresh(), 2);
        let _a = registry.register(a).unwrap();
        let id = registry
            .inner
            .sweeper
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .id();
        let (b, _b_events) = context(RunId::fresh(), 8);
        let _b = registry.register(b).unwrap();
        assert_eq!(
            registry
                .inner
                .sweeper
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .id(),
            id
        );
        first.changed().await.unwrap();
        second.changed().await.unwrap();
        assert_eq!(*first.borrow_and_update(), *second.borrow_and_update());
    }
    #[tokio::test(start_paused = true)]
    async fn external_outbox_publication_is_found_without_a_local_notification() {
        use autra_core::{EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus};
        use autra_execution::EffectJournal;
        let registry = RunContextRegistry::default();
        let run = RunId::fresh();
        let journal = Arc::new(crate::MemoryJournal::new());
        let (mut context, receiver) = context(run.clone(), 8);
        let tools = ToolRegistry::new();
        context.executor = LocalEngine::new(
            Arc::new(LocalDispatcher::new(Arc::new(Model), tools.clone())),
            tools,
        )
        .with_journal(journal.clone())
        .effect_executor();
        let ready = context.result_ready.clone();
        let registration = registry.register(context).unwrap();
        let worker =
            super::super::pool::spawn(registration.context.clone(), registry.recovery_ticks());
        tokio::time::sleep(Duration::from_millis(1)).await;
        let mut effect = EffectRecord::new(
            EffectId::fresh(),
            EffectKind::ModelCall,
            None,
            None,
            "digest".into(),
        )
        .with_intent_json(
            serde_json::to_string(&EffectIntent::CallModel(ModelCallSpec {
                system: None,
                messages: vec![],
                tools: vec![],
            }))
            .unwrap(),
        );
        effect.status = EffectStatus::Authorized;
        journal.authorize_effect(&run, &effect).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(journal.pending_results(&run, 10).unwrap().is_empty());
        ready.notified().await;
        assert_eq!(journal.pending_results(&run, 10).unwrap().len(), 1);
        drop(receiver);
        worker.await.unwrap();
        assert!(Arc::ptr_eq(
            &registry.resolve(&run).unwrap(),
            &registration.context,
        ));
        drop(registration);
        assert!(registry.resolve(&run).is_none());
    }
    #[test]
    fn recovery_clock_restarts_when_engine_is_reused_in_another_runtime() {
        let registry = RunContextRegistry::default();
        let first = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_registration, _receiver) = first.block_on(async {
            let (context, receiver) = context(RunId::fresh(), 2);
            (registry.register(context).unwrap(), receiver)
        });
        drop(first);
        let second = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        second.block_on(async {
            let mut ticks = registry.recovery_ticks();
            let (context, _receiver) = context(RunId::fresh(), 8);
            let _registration = registry.register(context).unwrap();
            tokio::time::timeout(Duration::from_secs(2), ticks.changed())
                .await
                .unwrap()
                .unwrap();
        });
    }
    #[tokio::test(start_paused = true)]
    async fn a_finished_effect_notifies_before_another_effect_in_the_batch_finishes() {
        use autra_core::{
            EffectClass, EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus,
            ToolCallRequest, ToolCallSpec, ToolSpec,
        };
        use autra_execution::{EffectJournal, Tool, ToolError};
        struct GateTool(Arc<Notify>);
        #[async_trait]
        impl Tool for GateTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "gate".into(),
                    description: "test".into(),
                    input_schema: "{}".into(),
                }
            }
            fn effect_class(&self) -> EffectClass {
                EffectClass::ReadOnly
            }
            async fn call(&self, args: &str) -> Result<String, ToolError> {
                if args.contains("slow") {
                    self.0.notified().await;
                }
                Ok("ok".into())
            }
        }
        let registry = RunContextRegistry::default();
        let run = RunId::fresh();
        let journal = Arc::new(crate::MemoryJournal::new());
        let gate = Arc::new(Notify::new());
        let mut tools = ToolRegistry::new();
        tools.register(GateTool(gate.clone()));
        let (mut context, receiver) = context(run.clone(), 8);
        context.max_parallel = 2;
        context.executor = LocalEngine::new(
            Arc::new(LocalDispatcher::new(Arc::new(Model), tools.clone())),
            tools,
        )
        .with_journal(journal.clone())
        .effect_executor();
        for kind in ["fast", "slow"] {
            let call = ToolCallRequest {
                call_id: kind.into(),
                name: "gate".into(),
                arguments: format!("{{\"kind\":\"{kind}\"}}"),
            };
            let mut effect = EffectRecord::new(
                EffectId::fresh(),
                EffectKind::ToolCall,
                Some("gate".into()),
                Some(kind.into()),
                "digest".into(),
            )
            .with_intent_json(
                serde_json::to_string(&EffectIntent::CallTool(ToolCallSpec { call })).unwrap(),
            );
            effect.status = EffectStatus::Authorized;
            journal.authorize_effect(&run, &effect).unwrap();
        }
        let ready = context.result_ready.clone();
        let registration = registry.register(context).unwrap();
        let worker =
            super::super::pool::spawn(registration.context.clone(), registry.recovery_ticks());
        tokio::time::timeout(Duration::from_millis(100), ready.notified())
            .await
            .unwrap();
        assert_eq!(journal.pending_results(&run, 10).unwrap().len(), 1);
        drop(registration);
        drop(receiver);
        assert!(registry.resolve(&run).is_some());
        gate.notify_one();
        tokio::time::timeout(Duration::from_millis(100), ready.notified())
            .await
            .unwrap();
        assert_eq!(journal.pending_results(&run, 10).unwrap().len(), 2);
        worker.await.unwrap();
        assert!(registry.resolve(&run).is_none());
    }
}

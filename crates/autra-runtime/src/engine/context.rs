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
    started: std::sync::atomic::AtomicBool,
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
                started: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }
}
impl RunContextRegistry {
    pub fn register(
        &self,
        context: RunExecutionContext,
    ) -> Result<ContextRegistration, autra_execution::RunError> {
        let context = Arc::new(context);
        {
            let mut contexts = self.inner.contexts.lock().expect("context lock poisoned");
            if contexts
                .get(&context.run_id)
                .and_then(Weak::upgrade)
                .is_some_and(|old| !old.events.is_closed())
            {
                return Err(autra_execution::RunError::Protocol(
                    "Run already has a live execution context".into(),
                ));
            }
            contexts.insert(context.run_id.clone(), Arc::downgrade(&context));
        }
        if !self
            .inner
            .started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let weak = Arc::downgrade(&self.inner);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(RECOVERY_SCAN_INTERVAL).await;
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    inner
                        .sweep
                        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
                }
            });
        }
        Ok(ContextRegistration {
            context,
            registry: self.clone(),
        })
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
// A worker holds the registration until committed Outbox work has drained.
// An old worker must never remove the replacement Coordinator's context.
pub(super) struct ContextRegistration {
    pub context: Arc<RunExecutionContext>,
    registry: RunContextRegistry,
}
impl Drop for ContextRegistration {
    fn drop(&mut self) {
        let mut contexts = self
            .registry
            .inner
            .contexts
            .lock()
            .expect("context lock poisoned");
        if contexts
            .get(&self.context.run_id)
            .is_some_and(|c| Weak::ptr_eq(c, &Arc::downgrade(&self.context)))
        {
            contexts.remove(&self.context.run_id);
        }
    }
}

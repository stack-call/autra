//! Owned per-Run executor. Results are durable; bounded notifications only wake
//! readers or carry lossy model deltas. Dropping the caller is not cancellation.
use super::EffectExecutor;
use crate::dispatcher::{DEFAULT_CLAIM_LEASE, DispatchOptions};
use autra_core::{BudgetHandle, BudgetSnapshot, CancelFlag, RunId};
use autra_execution::{ChildExecutionContext, RunEvent};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc::Sender};
pub(super) enum PoolEvent {
    Wake,
    Stream(RunEvent),
    Failed(String),
}
pub(super) struct WorkerContext {
    pub run_id: RunId,
    pub cancel: CancelFlag,
    pub budget: Arc<Mutex<BudgetSnapshot>>,
    pub events: Sender<PoolEvent>,
    pub wake: Arc<Notify>,
    pub timeout: Option<Duration>,
    pub max_parallel: usize,
    pub child_execution: ChildExecutionContext,
}
pub(super) fn spawn(
    executor: EffectExecutor,
    context: WorkerContext,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let budget =
                BudgetHandle::new(context.budget.lock().expect("budget lock poisoned").clone());
            let streams = context.events.clone();
            let options = DispatchOptions {
                limit: context.max_parallel,
                max_parallel: context.max_parallel,
                timeout: context.timeout,
                cancel: Some(&context.cancel),
                lease: DEFAULT_CLAIM_LEASE,
                budget: Some(budget),
                child_execution: Some(context.child_execution.clone()),
                delta: Some(Arc::new(move |text| {
                    let _ = streams.try_send(PoolEvent::Stream(RunEvent::ModelDelta { text }));
                })),
                scheduler: None,
                result_inbox: true,
            };
            let results = match executor.execute_pending(&context.run_id, options).await {
                Ok(rs) => rs,
                Err(e) => {
                    let _ = context.events.send(PoolEvent::Failed(e.to_string())).await;
                    return;
                }
            };
            if !results.is_empty() {
                let _ = context.events.try_send(PoolEvent::Wake);
            }
            if context.events.is_closed() {
                if results.is_empty() {
                    return;
                }
                continue;
            }
            tokio::select! {_=context.wake.notified()=>{},_=tokio::time::sleep(Duration::from_millis(20))=>{},_=context.events.closed()=>{}}
        }
    })
}

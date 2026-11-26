//! Worker lifecycle, separate from registered Run execution context.
//! Notifications are hints; durable Inbox/Outbox and recovery ticks are authoritative.
use super::context::{ContextRegistration, RunContextRegistry};
use crate::dispatcher::{DEFAULT_CLAIM_LEASE, DispatchOptions};
use autra_execution::RunEvent;
use std::sync::Arc;
pub(super) enum PoolEvent {
    Stream(RunEvent),
    Failed(String),
}
pub(super) fn spawn(
    registration: ContextRegistration,
    registry: RunContextRegistry,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let context = registration.context.clone();
        let mut recovery = registry.recovery_ticks();
        let mut cancellation_seen = false;
        loop {
            // Resolve by identity, while retaining this registration's profile
            // if a replacement Coordinator has registered a new generation.
            let resolved = registry.resolve(&context.run_id);
            let context = resolved
                .filter(|c| Arc::ptr_eq(c, &context))
                .unwrap_or_else(|| context.clone());
            let work = context.work_ready.notified();
            tokio::pin!(work);
            work.as_mut().enable();
            let streams = context.events.clone();
            let options = DispatchOptions {
                limit: context.max_parallel,
                max_parallel: context.max_parallel,
                timeout: context.timeout,
                cancel: Some(&context.cancel),
                lease: DEFAULT_CLAIM_LEASE,
                budget: Some(context.budget.clone()),
                child_execution: Some(context.child_execution.clone()),
                delta: Some(Arc::new(move |text| {
                    let _ = streams.try_send(PoolEvent::Stream(RunEvent::ModelDelta { text }));
                })),
                scheduler: None,
                result_inbox: true,
            };
            let results = match context
                .executor
                .execute_pending_notifying(&context.run_id, options, &context.result_ready)
                .await
            {
                Ok(results) => results,
                Err(error) => {
                    let _ = context
                        .events
                        .send(PoolEvent::Failed(error.to_string()))
                        .await;
                    return;
                }
            };
            if context.events.is_closed() {
                if results.is_empty() {
                    return;
                }
                continue;
            }
            // Drain every committed batch before sleeping, including detached work.
            if !results.is_empty() {
                continue;
            }
            tokio::select! {
                _ = &mut work => {},
                _ = recovery.changed() => {},
                _ = context.events.closed() => {},
                _ = context.cancel.cancelled(), if !cancellation_seen => { cancellation_seen = true; },
            }
        }
    })
}

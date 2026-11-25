//! Executor loop: claim authorized Effects from the outbox, execute them and
//! report the result back as an event.
//!
//! The coordinator commits a transition and then waits on its inbox; this loop
//! is the only place that calls the dispatcher. That is the message boundary
//! described in `docs/message-driven-runtime.md`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use autra_core::{
    BudgetHandle, BudgetSnapshot, CancelFlag, EffectId, EffectKind, EventKind, RunId,
};
use autra_execution::{ChildExecutionContext, EffectJournal};
use tokio::sync::{Notify, mpsc::UnboundedSender};

use crate::dispatcher::{DEFAULT_CLAIM_LEASE, DispatchOptions, DispatchOutcome, OutboxDispatcher};

use super::LocalEngine;
use super::tool_outcome_from_record;

/// What the executor loop reports back to the coordinator.
pub(super) enum PoolEvent {
    /// A result that becomes an accepted event, plus the Effect it resolved.
    Accepted(EffectId, EventKind),
    /// The model call failed; the coordinator must fail the Run.
    ModelFailed(String),
    /// The effect was cancelled before dispatch.
    Cancelled(EffectId),
}

const POOL_BATCH: usize = 16;
const IDLE_POLL: Duration = Duration::from_millis(20);

/// Spawn the executor loop for one journaled Run.
///
/// The loop exits when the coordinator drops the receiver, which happens when
/// `drive` returns.
pub(super) fn spawn(
    engine: &LocalEngine,
    run_id: RunId,
    cancel: CancelFlag,
    budget: Arc<Mutex<BudgetSnapshot>>,
    events: UnboundedSender<PoolEvent>,
    wake: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    let Some(journal) = engine.journal.clone() else {
        return tokio::spawn(async {});
    };
    let dispatcher = engine.dispatcher.clone();
    let scheduler = engine.scheduler.clone();
    let effect_timeout = engine.effect_timeout;
    let child_execution = ChildExecutionContext {
        journal: engine.journal.clone(),
        approver: engine.approver.clone(),
        depth: 0,
    };
    tokio::spawn(async move {
        loop {
            let snapshot = budget.lock().expect("budget lock poisoned").clone();
            let options = DispatchOptions {
                limit: POOL_BATCH,
                max_parallel: 4,
                timeout: effect_timeout,
                cancel: Some(&cancel),
                lease: DEFAULT_CLAIM_LEASE,
                budget: Some(BudgetHandle::new(snapshot)),
                child_execution: Some(child_execution.clone()),
                delta: None,
                scheduler: Some(scheduler.clone()),
            };
            let results = match OutboxDispatcher::run_once_for_run_with(
                journal.journal(),
                &run_id,
                options,
                dispatcher.as_ref(),
                |_| {},
            )
            .await
            {
                Ok(results) => results,
                Err(error) => {
                    let _ = events.send(PoolEvent::ModelFailed(format!(
                        "executor loop failed: {error}"
                    )));
                    return;
                }
            };

            let mut sent = false;
            for result in results {
                let effect_id = result.effect_id.clone();
                let message = match result.outcome {
                    DispatchOutcome::Model(Ok(output)) => {
                        PoolEvent::Accepted(effect_id, EventKind::ModelResult(output))
                    }
                    DispatchOutcome::Model(Err(error)) => PoolEvent::ModelFailed(error.to_string()),
                    DispatchOutcome::Tool(outcome) => {
                        PoolEvent::Accepted(effect_id, EventKind::ToolResult(outcome))
                    }
                    DispatchOutcome::Cancelled => PoolEvent::Cancelled(effect_id),
                    DispatchOutcome::AlreadyResolved | DispatchOutcome::Interrupted => {
                        match resolved_event(journal.journal(), &run_id, &effect_id) {
                            Some(kind) => PoolEvent::Accepted(effect_id, kind),
                            None => continue,
                        }
                    }
                };
                sent = true;
                if events.send(message).is_err() {
                    return;
                }
            }

            if !sent {
                tokio::select! {
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(IDLE_POLL) => {}
                }
            }
        }
    })
}

fn resolved_event(
    journal: &dyn EffectJournal,
    run_id: &RunId,
    effect_id: &EffectId,
) -> Option<EventKind> {
    let record = journal.load_effect(run_id, effect_id).ok()??;
    if record.kind != EffectKind::ToolCall {
        return None;
    }
    tool_outcome_from_record(&record).map(EventKind::ToolResult)
}

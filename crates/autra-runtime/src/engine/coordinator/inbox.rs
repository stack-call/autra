//! The single event entry for one Run.
//!
//! Every input the coordinator consumes goes through here: model results, tool
//! results, cancellation and executor-loop reports. Keeping one entry point is
//! what lets a Run later be parked and woken without changing the state machine.

use std::time::Duration;

use autra_core::{AcceptedEvent, CancelFlag, EffectId, EventKind, ModelOutput, ToolOutcome};
use autra_execution::RunError;
use tokio::sync::mpsc::UnboundedReceiver;

use super::pool::PoolEvent;
use super::result::ResultCollector;

const CANCEL_POLL: Duration = Duration::from_millis(20);

/// What the coordinator should do next.
pub(super) enum Next {
    /// An accepted event to feed to the stepper.
    Event(AcceptedEvent),
    /// A cancellation was requested; the coordinator must queue
    /// `CancelRequested` and drain the current Effect.
    Cancelled,
    /// The executor loop resolved an Effect; the event is already queued.
    PoolResult(EffectId, EventKind),
    /// The executor loop cancelled an Effect.
    PoolCancelled(EffectId),
    /// The executor loop reported a model failure.
    ModelFailed(String),
    /// The executor loop ended without producing anything.
    ExecutorEnded,
}

pub(super) struct RunInbox {
    collector: ResultCollector,
    pool: Option<UnboundedReceiver<PoolEvent>>,
    cancel: CancelFlag,
    cancel_reported: bool,
}

impl RunInbox {
    pub(super) fn new(
        collector: ResultCollector,
        pool: Option<UnboundedReceiver<PoolEvent>>,
        cancel: CancelFlag,
    ) -> Self {
        Self {
            collector,
            pool,
            cancel,
            cancel_reported: false,
        }
    }

    pub(super) fn seq(&self) -> u64 {
        self.collector.seq()
    }

    pub(super) fn enqueue(&mut self, kind: EventKind) {
        self.collector.enqueue(kind);
    }

    pub(super) fn tool_result(&mut self, outcome: ToolOutcome) {
        self.collector.tool_result(outcome);
    }

    pub(super) fn model_result(&mut self, output: ModelOutput) {
        self.collector.model_result(output);
    }

    pub(super) fn cancel(&mut self) {
        self.collector.cancel();
        self.cancel_reported = true;
    }

    /// Wait for the next input. Queued events win; otherwise the inbox waits on
    /// the executor loop, on cancellation, or on neither.
    pub(super) async fn next(&mut self) -> Result<Next, RunError> {
        if let Some(event) = self.collector.pop_front() {
            return Ok(Next::Event(event));
        }
        loop {
            if self.cancel.is_cancelled() && !self.cancel_reported {
                self.cancel_reported = true;
                return Ok(Next::Cancelled);
            }

            match self.pool.as_mut() {
                Some(pool) => {
                    tokio::select! {
                        message = pool.recv() => match message {
                            Some(PoolEvent::Accepted(effect_id, kind)) => {
                                return Ok(Next::PoolResult(effect_id, kind));
                            }
                            Some(PoolEvent::Cancelled(effect_id)) => {
                                return Ok(Next::PoolCancelled(effect_id));
                            }
                            Some(PoolEvent::ModelFailed(message)) => {
                                return Ok(Next::ModelFailed(message));
                            }
                            None => return Ok(Next::ExecutorEnded),
                        },
                        _ = tokio::time::sleep(CANCEL_POLL) => {}
                    }
                }
                None => {
                    if !self.cancel.is_cancelled() {
                        return Err(RunError::Protocol(
                            "no accepted event to process".to_owned(),
                        ));
                    }
                    tokio::time::sleep(CANCEL_POLL).await;
                }
            }
        }
    }
}

//! One input entry. Journal results are authoritative; channels are wake hints.
use super::pool::PoolEvent;
use super::result::ResultCollector;
use autra_core::{AcceptedEvent, CancelFlag, EventKind, ModelOutput, RunId, ToolOutcome};
use autra_execution::{DurableTransitionStore, EffectResult, RunError, RunEvent};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
pub(super) enum Next {
    Event(AcceptedEvent),
    Cancelled,
    Result(EffectResult),
    Stream(RunEvent),
    ExecutorFailed(String),
}
pub(super) struct RunInbox {
    collector: ResultCollector,
    pool: Option<Receiver<PoolEvent>>,
    journal: Option<Arc<dyn DurableTransitionStore>>,
    run_id: RunId,
    cancel: CancelFlag,
    cancel_reported: bool,
    executor_ended: bool,
}
impl RunInbox {
    pub(super) fn new(
        mut collector: ResultCollector,
        pool: Option<Receiver<PoolEvent>>,
        journal: Option<Arc<dyn DurableTransitionStore>>,
        run_id: RunId,
        cancel: CancelFlag,
        last_seq: u64,
    ) -> Self {
        collector.advance_seq(last_seq);
        Self {
            collector,
            pool,
            journal,
            run_id,
            cancel,
            cancel_reported: false,
            executor_ended: false,
        }
    }
    pub(super) fn seq(&self) -> u64 {
        self.collector.seq()
    }
    pub(super) fn enqueue(&mut self, kind: EventKind) {
        self.collector.enqueue(kind);
    }
    pub(super) fn tool_result(&mut self, o: ToolOutcome) {
        self.collector.tool_result(o);
    }
    pub(super) fn model_result(&mut self, o: ModelOutput) {
        self.collector.model_result(o);
    }
    pub(super) fn cancel(&mut self) {
        self.collector.cancel();
        self.cancel_reported = true;
    }
    pub(super) async fn next(&mut self) -> Result<Next, RunError> {
        if let Some(e) = self.collector.pop_front() {
            return Ok(Next::Event(e));
        }
        loop {
            if let Some(j) = &self.journal {
                if let Some(r) = j
                    .pending_results(&self.run_id, 1)
                    .map_err(|e| RunError::Journal(e.to_string()))?
                    .into_iter()
                    .next()
                {
                    return Ok(Next::Result(r));
                }
            }
            if self.executor_ended {
                return Err(RunError::Protocol(
                    "executor ended before publishing a result".into(),
                ));
            }
            if self.cancel.is_cancelled() && !self.cancel_reported {
                self.cancel_reported = true;
                return Ok(Next::Cancelled);
            }
            match self.pool.as_mut() {
                Some(pool) => tokio::select! {
                    message=pool.recv()=>match message {
                        Some(PoolEvent::Wake)=>{},Some(PoolEvent::Stream(e))=>return Ok(Next::Stream(e)),Some(PoolEvent::Failed(m))=>return Ok(Next::ExecutorFailed(m)),None=>{self.pool=None;self.executor_ended=true;}
                    },
                    _=tokio::time::sleep(Duration::from_millis(20))=>{}
                },
                None => {
                    if !self.cancel.is_cancelled() {
                        return Err(RunError::Protocol("no accepted event to process".into()));
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    }
}

//! One input entry. Journal results are authoritative; channels are wake hints.
use super::pool::PoolEvent;
use super::result::ResultCollector;
use autra_core::{AcceptedEvent, CancelFlag, EventKind, ModelOutput, RunId, ToolOutcome};
use autra_execution::{DurableTransitionStore, EffectResult, RunError, RunEvent};
use std::sync::Arc;
use tokio::sync::{Notify, mpsc::Receiver, watch};
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
    scan_results: bool,
    wakeup: Option<(Arc<Notify>, watch::Receiver<u64>)>,
}
impl RunInbox {
    pub(super) fn new(
        mut collector: ResultCollector,
        pool: Option<Receiver<PoolEvent>>,
        journal: Option<Arc<dyn DurableTransitionStore>>,
        run_id: RunId,
        cancel: CancelFlag,
        last_seq: u64,
        wakeup: Option<(Arc<Notify>, watch::Receiver<u64>)>,
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
            scan_results: true,
            wakeup,
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
            if self.scan_results {
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
            }
            self.scan_results = false;
            if self.executor_ended {
                return Err(RunError::Protocol(
                    "executor ended before publishing a result".into(),
                ));
            }
            if self.cancel.is_cancelled() && !self.cancel_reported {
                self.cancel_reported = true;
                return Ok(Next::Cancelled);
            }
            match (self.pool.as_mut(), self.wakeup.as_mut()) {
                (Some(pool), Some((ready, recovery))) => tokio::select! {
                    message = pool.recv() => match message {
                        Some(PoolEvent::Stream(event)) => return Ok(Next::Stream(event)),
                        Some(PoolEvent::Failed(message)) => return Ok(Next::ExecutorFailed(message)),
                        None => { self.pool = None; self.executor_ended = true; self.scan_results = true; }
                    },
                    _ = ready.notified() => { self.scan_results = true; },
                    _ = recovery.changed() => { self.scan_results = true; },
                    _ = self.cancel.cancelled(), if !self.cancel_reported => {},
                },
                _ => return Err(RunError::Protocol("no accepted event to process".into())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryJournal;
    use autra_core::{EffectId, EffectKind, EffectRecord, EffectStatus};
    use autra_execution::{EffectJournal, EffectOutcome, ResultIdentity};
    use std::collections::VecDeque;
    use std::time::Duration;
    struct Fixture {
        journal: Arc<MemoryJournal>,
        run: RunId,
        sender: tokio::sync::mpsc::Sender<PoolEvent>,
        ready: Arc<Notify>,
        sweep: watch::Sender<u64>,
        inbox: RunInbox,
    }
    fn fixture() -> Fixture {
        let journal = Arc::new(MemoryJournal::new());
        let run = RunId::fresh();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let ready = Arc::new(Notify::new());
        let (sweep, recovery) = watch::channel(0);
        let inbox = RunInbox::new(
            ResultCollector::new(VecDeque::new()),
            Some(receiver),
            Some(journal.clone()),
            run.clone(),
            CancelFlag::new(),
            0,
            Some((ready.clone(), recovery)),
        );
        Fixture {
            journal,
            run,
            sender,
            ready,
            sweep,
            inbox,
        }
    }
    fn publish(journal: &MemoryJournal, run: &RunId) -> EffectResult {
        let mut effect = EffectRecord::new(
            EffectId::fresh(),
            EffectKind::ModelCall,
            None,
            None,
            "digest".into(),
        );
        effect.intent_json = serde_json::to_string(&autra_core::EffectIntent::CallModel(
            autra_core::ModelCallSpec {
                system: None,
                messages: vec![],
                tools: vec![],
            },
        ))
        .unwrap();
        effect.status = EffectStatus::Authorized;
        journal.record(run, &effect).unwrap();
        journal.enqueue_effect(run, &effect).unwrap();
        journal
            .claim_effect(&effect.id, "worker", Duration::from_secs(60))
            .unwrap()
            .unwrap();
        effect.status = EffectStatus::Committed;
        let result = EffectResult {
            identity: ResultIdentity {
                effect_id: effect.id.clone(),
                attempt_id: "worker".into(),
            },
            outcome: EffectOutcome::Model(ModelOutput {
                text: Some("durable".into()),
                tool_calls: vec![],
                usage: None,
            }),
            budget: None,
        };
        journal
            .record_result_claimed(run, &effect, "worker", &result)
            .unwrap();
        result
    }
    #[tokio::test(start_paused = true)]
    async fn missed_result_notification_is_recovered_by_the_shared_tick() {
        let mut f = fixture();
        let next = f.inbox.next();
        tokio::pin!(next);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut next)
                .await
                .is_err()
        );
        let expected = publish(&f.journal, &f.run);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut next)
                .await
                .is_err()
        );
        f.sweep.send_modify(|epoch| *epoch += 1);
        assert!(matches!(next.await.unwrap(),Next::Result(result) if result==expected));
    }
    #[tokio::test(start_paused = true)]
    async fn publication_between_empty_read_and_wait_is_not_lost() {
        let mut f = fixture();
        let next = f.inbox.next();
        tokio::pin!(next);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut next)
                .await
                .is_err()
        );
        let expected = publish(&f.journal, &f.run);
        f.ready.notify_one();
        assert!(matches!(next.await.unwrap(),Next::Result(result) if result==expected));
    }
    #[tokio::test(start_paused = true)]
    async fn stream_deltas_do_not_poll_durable_inbox() {
        let mut f = fixture();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), f.inbox.next())
                .await
                .is_err()
        );
        let expected = publish(&f.journal, &f.run);
        for text in ["one", "two", "three"] {
            f.sender
                .send(PoolEvent::Stream(RunEvent::ModelDelta {
                    text: text.into(),
                }))
                .await
                .unwrap();
            assert!(
                matches!(f.inbox.next().await.unwrap(),Next::Stream(RunEvent::ModelDelta{text:delta}) if delta==text)
            );
        }
        f.ready.notify_one();
        assert!(matches!(f.inbox.next().await.unwrap(),Next::Result(result) if result==expected));
    }
    #[tokio::test(start_paused = true)]
    async fn parent_cancel_wakes_inbox_without_recovery_tick() {
        let mut f = fixture();
        let parent = CancelFlag::new();
        f.inbox.cancel = CancelFlag::child_of(&parent);
        let next = f.inbox.next();
        tokio::pin!(next);
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut next)
                .await
                .is_err()
        );
        parent.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(1), next)
                .await
                .unwrap()
                .unwrap(),
            Next::Cancelled
        ));
    }
}

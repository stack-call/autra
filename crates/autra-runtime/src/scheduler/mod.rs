//! Global resource scheduling across Runs.
//!
//! One bounded, fair pool per resource. The scheduler decides *when* an
//! already-authorized Effect may execute; it never changes the Effect or the
//! Run state.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use autra_core::{CancelFlag, RunId};
use tokio::sync::oneshot;

/// Resource pools a local executor can constrain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Model,
    Tool,
    Process,
}

/// Per-pool concurrency limits and a bounded wait queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerLimits {
    pub model: usize,
    pub tool: usize,
    pub process: usize,
    pub max_waiters: usize,
}

impl Default for SchedulerLimits {
    fn default() -> Self {
        Self {
            model: 16,
            tool: 64,
            process: 8,
            max_waiters: 1024,
        }
    }
}

impl SchedulerLimits {
    pub fn new(model: usize, tool: usize, process: usize) -> Self {
        Self {
            model: model.max(1),
            tool: tool.max(1),
            process: process.max(1),
            ..Self::default()
        }
    }

    pub fn with_max_waiters(mut self, max_waiters: usize) -> Self {
        self.max_waiters = max_waiters.max(1);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireError {
    Cancelled,
    QueueFull,
}

struct Waiter {
    id: u64,
    run_id: RunId,
    sender: oneshot::Sender<()>,
}

struct PoolState {
    active: usize,
    queue: VecDeque<Waiter>,
    last_run: Option<RunId>,
}

struct FairPool {
    limit: usize,
    max_waiters: usize,
    state: Mutex<PoolState>,
}

impl FairPool {
    fn new(limit: usize, max_waiters: usize) -> Self {
        Self {
            limit: limit.max(1),
            max_waiters: max_waiters.max(1),
            state: Mutex::new(PoolState {
                active: 0,
                queue: VecDeque::new(),
                last_run: None,
            }),
        }
    }

    fn next_waiter(state: &mut PoolState) -> Option<Waiter> {
        if state.queue.is_empty() {
            return None;
        }
        let index = state
            .queue
            .iter()
            .position(|waiter| state.last_run.as_ref() != Some(&waiter.run_id))
            .unwrap_or(0);
        state.queue.remove(index)
    }

    async fn acquire(
        self: &Arc<Self>,
        run_id: RunId,
        cancel: Option<&CancelFlag>,
    ) -> Result<SchedulerPermit, AcquireError> {
        let (mut receiver, waiter_id) = {
            let mut state = self.state.lock().expect("scheduler lock poisoned");
            if state.active < self.limit && state.queue.is_empty() {
                state.active += 1;
                state.last_run = Some(run_id);
                return Ok(SchedulerPermit { pool: self.clone() });
            }
            if state.queue.len() >= self.max_waiters {
                return Err(AcquireError::QueueFull);
            }
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let (sender, receiver) = oneshot::channel();
            state.queue.push_back(Waiter { id, run_id, sender });
            (receiver, id)
        };

        let Some(cancel) = cancel else {
            return receiver
                .await
                .map(|()| SchedulerPermit { pool: self.clone() })
                .map_err(|_| AcquireError::Cancelled);
        };
        if cancel.is_cancelled() {
            self.cancel_waiter(waiter_id, receiver).await;
            return Err(AcquireError::Cancelled);
        }

        loop {
            tokio::select! {
                result = &mut receiver => {
                    return result
                        .map(|()| SchedulerPermit { pool: self.clone() })
                        .map_err(|_| AcquireError::Cancelled);
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if cancel.is_cancelled() {
                        self.cancel_waiter(waiter_id, receiver).await;
                        return Err(AcquireError::Cancelled);
                    }
                }
            }
        }
    }

    /// Remove a cancelled waiter. If a release already handed it a permit, give
    /// that permit back before reporting cancellation.
    async fn cancel_waiter(self: &Arc<Self>, waiter_id: u64, receiver: oneshot::Receiver<()>) {
        let removed = {
            let mut state = self.state.lock().expect("scheduler lock poisoned");
            let index = state.queue.iter().position(|waiter| waiter.id == waiter_id);
            if let Some(index) = index {
                state.queue.remove(index);
            }
            index.is_some()
        };
        if !removed && receiver.await.is_ok() {
            // The release raced with cancellation and granted the slot.
            drop(SchedulerPermit { pool: self.clone() });
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("scheduler lock poisoned");
        loop {
            let Some(waiter) = Self::next_waiter(&mut state) else {
                state.active = state.active.saturating_sub(1);
                state.last_run = None;
                return;
            };
            state.last_run = Some(waiter.run_id.clone());
            if waiter.sender.send(()).is_ok() {
                return;
            }
            // The waiter cancelled between being selected and notified.
        }
    }
}

/// Global scheduler shared by every Run driven by one engine.
pub struct GlobalScheduler {
    model: Arc<FairPool>,
    tool: Arc<FairPool>,
    process: Arc<FairPool>,
}

impl GlobalScheduler {
    pub fn new(limits: SchedulerLimits) -> Self {
        Self {
            model: Arc::new(FairPool::new(limits.model, limits.max_waiters)),
            tool: Arc::new(FairPool::new(limits.tool, limits.max_waiters)),
            process: Arc::new(FairPool::new(limits.process, limits.max_waiters)),
        }
    }

    fn pool(&self, kind: ResourceKind) -> Arc<FairPool> {
        match kind {
            ResourceKind::Model => self.model.clone(),
            ResourceKind::Tool => self.tool.clone(),
            ResourceKind::Process => self.process.clone(),
        }
    }

    /// Wait for a resource permit. A cancelled Run stops waiting; a full queue
    /// returns `QueueFull` instead of growing without bound. The permit is
    /// released on drop.
    pub async fn acquire(
        &self,
        kind: ResourceKind,
        run_id: &RunId,
        cancel: Option<&CancelFlag>,
    ) -> Result<SchedulerPermit, AcquireError> {
        self.pool(kind).acquire(run_id.clone(), cancel).await
    }
}

impl Default for GlobalScheduler {
    fn default() -> Self {
        Self::new(SchedulerLimits::default())
    }
}

/// RAII permit. Dropping it hands the slot to the next waiter.
pub struct SchedulerPermit {
    pool: Arc<FairPool>,
}

impl Drop for SchedulerPermit {
    fn drop(&mut self) {
        self.pool.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str) -> RunId {
        RunId::new(id)
    }

    #[tokio::test]
    async fn a_pool_blocks_after_its_limit_and_releases_on_drop() {
        let scheduler = GlobalScheduler::new(SchedulerLimits::new(1, 1, 1));

        let first = scheduler
            .acquire(ResourceKind::Model, &run("r1"), None)
            .await
            .unwrap();
        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            scheduler.acquire(ResourceKind::Model, &run("r2"), None),
        )
        .await;
        assert!(blocked.is_err());

        drop(first);
        let next = tokio::time::timeout(
            Duration::from_millis(50),
            scheduler.acquire(ResourceKind::Model, &run("r2"), None),
        )
        .await;
        assert!(next.is_ok());
    }

    #[tokio::test]
    async fn a_cancelled_waiter_stops() {
        let scheduler = GlobalScheduler::new(SchedulerLimits::new(1, 1, 1));
        let _held = scheduler
            .acquire(ResourceKind::Tool, &run("holder"), None)
            .await
            .unwrap();
        let cancel = CancelFlag::new();
        let cancel_for_task = cancel.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_for_task.cancel();
        });

        let permit = scheduler
            .acquire(ResourceKind::Tool, &run("waiter"), Some(&cancel))
            .await;
        task.await.unwrap();

        assert!(matches!(permit, Err(AcquireError::Cancelled)));
    }

    #[tokio::test]
    async fn a_full_wait_queue_is_rejected() {
        let scheduler = GlobalScheduler::new(SchedulerLimits::new(1, 1, 1).with_max_waiters(1));
        let _held = scheduler
            .acquire(ResourceKind::Tool, &run("holder"), None)
            .await
            .unwrap();
        let one = run("one");
        let first = scheduler.acquire(ResourceKind::Tool, &one, None);
        tokio::pin!(first);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            _ = &mut first => panic!("the first waiter should queue"),
        }
        let second = scheduler
            .acquire(ResourceKind::Tool, &run("two"), None)
            .await;
        assert!(matches!(second, Err(AcquireError::QueueFull)));
    }
}

#[cfg(test)]
mod fairness_tests {
    use super::*;

    #[tokio::test]
    async fn a_different_run_is_preferred_over_the_holder_reacquiring() {
        let scheduler = GlobalScheduler::new(SchedulerLimits::new(1, 1, 1));
        let run_a = RunId::new("a");
        let run_b = RunId::new("b");
        let holder = scheduler
            .acquire(ResourceKind::Tool, &run_a, None)
            .await
            .unwrap();
        let a_again = scheduler.acquire(ResourceKind::Tool, &run_a, None);
        tokio::pin!(a_again);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            _ = &mut a_again => panic!("A should be queued"),
        }
        let b = scheduler.acquire(ResourceKind::Tool, &run_b, None);
        tokio::pin!(b);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            _ = &mut b => panic!("B should be queued"),
        }

        drop(holder);
        let granted = tokio::time::timeout(Duration::from_millis(50), &mut b).await;
        assert!(granted.is_ok(), "B should receive the freed slot before A");
        let still_queued = tokio::time::timeout(Duration::from_millis(20), &mut a_again).await;
        assert!(still_queued.is_err(), "A must wait while B holds the slot");
    }
}

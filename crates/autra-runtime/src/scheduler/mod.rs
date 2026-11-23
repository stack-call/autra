//! Global resource scheduling across Runs.
//!
//! This is intentionally a local, in-process scheduler: one semaphore per
//! resource pool. It decides *when* an already-authorized Effect may execute;
//! it never changes the Effect or the Run state.

use std::sync::Arc;
use std::time::Duration;

use autra_core::CancelFlag;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Resource pools a local executor can constrain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Model,
    Tool,
    Process,
}

/// Per-pool concurrency limits. Every limit is normalized to at least 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerLimits {
    pub model: usize,
    pub tool: usize,
    pub process: usize,
}

impl Default for SchedulerLimits {
    fn default() -> Self {
        Self {
            model: 16,
            tool: 64,
            process: 8,
        }
    }
}

impl SchedulerLimits {
    pub fn new(model: usize, tool: usize, process: usize) -> Self {
        Self {
            model: model.max(1),
            tool: tool.max(1),
            process: process.max(1),
        }
    }
}

/// Global scheduler shared by every Run driven by one engine.
pub struct GlobalScheduler {
    model: Arc<Semaphore>,
    tool: Arc<Semaphore>,
    process: Arc<Semaphore>,
}

impl GlobalScheduler {
    pub fn new(limits: SchedulerLimits) -> Self {
        Self {
            model: Arc::new(Semaphore::new(limits.model.max(1))),
            tool: Arc::new(Semaphore::new(limits.tool.max(1))),
            process: Arc::new(Semaphore::new(limits.process.max(1))),
        }
    }

    fn semaphore(&self, kind: ResourceKind) -> Arc<Semaphore> {
        match kind {
            ResourceKind::Model => self.model.clone(),
            ResourceKind::Tool => self.tool.clone(),
            ResourceKind::Process => self.process.clone(),
        }
    }

    /// Wait for a resource permit. A cancelled Run stops waiting instead of
    /// occupying a queue slot. The permit is released on drop.
    pub async fn acquire(
        &self,
        kind: ResourceKind,
        cancel: Option<&CancelFlag>,
    ) -> Option<OwnedSemaphorePermit> {
        let semaphore = self.semaphore(kind);
        let Some(cancel) = cancel else {
            return semaphore.acquire_owned().await.ok();
        };
        if cancel.is_cancelled() {
            return None;
        }
        let acquire = semaphore.acquire_owned();
        tokio::pin!(acquire);
        loop {
            tokio::select! {
                permit = &mut acquire => return permit.ok(),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if cancel.is_cancelled() {
                        return None;
                    }
                }
            }
        }
    }
}

impl Default for GlobalScheduler {
    fn default() -> Self {
        Self::new(SchedulerLimits::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_pool_blocks_after_its_limit_and_releases_on_drop() {
        let scheduler = GlobalScheduler::new(SchedulerLimits::new(1, 1, 1));

        let first = scheduler.acquire(ResourceKind::Model, None).await.unwrap();
        let blocked = tokio::time::timeout(
            Duration::from_millis(50),
            scheduler.acquire(ResourceKind::Model, None),
        )
        .await;
        assert!(blocked.is_err());

        drop(first);
        let next = tokio::time::timeout(
            Duration::from_millis(50),
            scheduler.acquire(ResourceKind::Model, None),
        )
        .await;
        assert!(next.is_ok());
    }

    #[tokio::test]
    async fn a_cancelled_waiter_stops() {
        let scheduler = GlobalScheduler::new(SchedulerLimits::new(1, 1, 1));
        let _held = scheduler.acquire(ResourceKind::Tool, None).await.unwrap();
        let cancel = CancelFlag::new();
        let cancel_for_task = cancel.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_for_task.cancel();
        });

        let permit = scheduler.acquire(ResourceKind::Tool, Some(&cancel)).await;
        task.await.unwrap();

        assert!(permit.is_none());
    }
}

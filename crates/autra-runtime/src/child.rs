//! Child Run handles and join policies.

use std::sync::Arc;

use autra_core::{AgentStepper, BudgetSnapshot, CancelFlag, HistoryItem, RunId};
use autra_execution::{RunError, RunOutcome};

/// Everything needed to start a child Run on an existing engine.
#[derive(Clone)]
pub struct ChildSpec {
    pub stepper: Arc<dyn AgentStepper>,
    pub input: String,
    pub history: Vec<HistoryItem>,
    pub budget: BudgetSnapshot,
    /// The Run that spawned this child.
    pub parent_run_id: RunId,
    /// The parent's cancellation flag. The child flag inherits it.
    pub cancel: CancelFlag,
}

/// A running child. Dropping the handle does not cancel the child.
pub struct ChildHandle {
    run_id: RunId,
    cancel: CancelFlag,
    join: tokio::task::JoinHandle<Result<RunOutcome, RunError>>,
}

impl ChildHandle {
    pub(crate) fn new(
        run_id: RunId,
        cancel: CancelFlag,
        join: tokio::task::JoinHandle<Result<RunOutcome, RunError>>,
    ) -> Self {
        Self {
            run_id,
            cancel,
            join,
        }
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// Cancel only this child. The parent is unaffected.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub async fn join(self) -> Result<RunOutcome, RunError> {
        self.join
            .await
            .map_err(|error| RunError::Protocol(format!("child run task failed: {error}")))?
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinPolicy {
    /// Wait for every child and return all results.
    WaitAll,
    /// Stop at the first error and cancel the remaining children.
    FailFast,
}

/// A set of already-spawned children. Dropping unjoined children detaches
/// their tasks; use `cancel` on their handles to stop them.
pub struct ChildRunSet {
    policy: JoinPolicy,
    children: Vec<ChildHandle>,
}

impl ChildRunSet {
    pub fn new(children: Vec<ChildHandle>, policy: JoinPolicy) -> Self {
        Self { policy, children }
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    pub async fn join(self) -> Vec<Result<RunOutcome, RunError>> {
        let mut results = Vec::with_capacity(self.children.len());
        let mut children = self.children.into_iter();
        while let Some(child) = children.next() {
            let result = child.join().await;
            let failed = result.is_err();
            results.push(result);
            if failed && self.policy == JoinPolicy::FailFast {
                for remaining in children {
                    remaining.cancel();
                }
                break;
            }
        }
        results
    }
}

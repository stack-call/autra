use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use serde::{Deserialize, Serialize};

use crate::ids::RunId;

/// Budget accounting for one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub max_turns: u32,
    pub turns_used: u32,
    pub max_tool_calls: u32,
    pub tool_calls_used: u32,
}

impl BudgetSnapshot {
    pub fn new(max_turns: u32, max_tool_calls: u32) -> Self {
        Self {
            max_turns,
            turns_used: 0,
            max_tool_calls,
            tool_calls_used: 0,
        }
    }

    pub fn turn_available(&self) -> bool {
        self.turns_used < self.max_turns
    }

    pub fn tool_calls_available(&self, requested: u32) -> bool {
        self.tool_calls_used.saturating_add(requested) <= self.max_tool_calls
    }
}

/// Cooperative cancellation shared between a caller, its children and the host.
///
/// A child flag is cancelled when its parent is cancelled; the reverse is not
/// true, so a child can be cancelled without stopping the parent.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag {
    state: Arc<CancellationState>,
    parent: Option<Arc<CancelFlag>>,
}

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a child scope flag that inherits parent cancellation.
    pub fn child_of(parent: &CancelFlag) -> Self {
        Self {
            state: Arc::new(CancellationState::default()),
            parent: Some(Arc::new(parent.clone())),
        }
    }

    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::SeqCst) {
            let waiters =
                std::mem::take(&mut *self.state.waiters.lock().expect("cancel lock poisoned"));
            for waker in waiters.into_values() {
                waker.wake();
            }
        }
    }

    /// Wait without polling. Uses only standard-library futures and wakers;
    /// parent cancellation wakes child waiters, never the reverse.
    pub fn cancelled(&self) -> Cancellation {
        Cancellation {
            flag: self.clone(),
            registrations: Vec::new(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
    }
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    next_waiter: AtomicU64,
    waiters: Mutex<BTreeMap<u64, Waker>>,
}

/// Runtime-independent cancellation future. Dropping it removes registrations.
#[derive(Debug)]
pub struct Cancellation {
    flag: CancelFlag,
    registrations: Vec<(Arc<CancellationState>, u64)>,
}
impl Cancellation {
    fn unregister(&mut self) {
        for (state, id) in self.registrations.drain(..) {
            let removed = state
                .waiters
                .lock()
                .expect("cancel lock poisoned")
                .remove(&id);
            drop(removed);
        }
    }
}
impl Future for Cancellation {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if this.flag.is_cancelled() {
            this.unregister();
            return Poll::Ready(());
        }
        if this.registrations.is_empty() {
            let mut flag = Some(&this.flag);
            while let Some(f) = flag {
                let id = f.state.next_waiter.fetch_add(1, Ordering::Relaxed);
                let removed = f
                    .state
                    .waiters
                    .lock()
                    .expect("cancel lock poisoned")
                    .insert(id, cx.waker().clone());
                drop(removed);
                this.registrations.push((f.state.clone(), id));
                flag = f.parent.as_deref();
            }
        } else {
            for (state, id) in &this.registrations {
                let removed = state
                    .waiters
                    .lock()
                    .expect("cancel lock poisoned")
                    .insert(*id, cx.waker().clone());
                drop(removed);
            }
        }
        // Close the cancel-versus-registration race.
        if this.flag.is_cancelled() {
            this.unregister();
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Permission, budget and cancellation identity for a run.
#[derive(Clone, Debug)]
pub struct ExecutionScope {
    pub run_id: RunId,
    pub parent_run_id: Option<RunId>,
    pub parent_scope_id: Option<String>,
    pub max_turns: u32,
    pub max_tool_calls: u32,
    pub cancellation: CancelFlag,
}

impl ExecutionScope {
    pub fn new(run_id: RunId, max_turns: u32, max_tool_calls: u32) -> Self {
        Self {
            run_id,
            parent_run_id: None,
            parent_scope_id: None,
            max_turns,
            max_tool_calls,
            cancellation: CancelFlag::new(),
        }
    }
}

/// A shared budget view for tool calls that spawn or reserve work.
#[derive(Clone, Debug)]
pub struct BudgetHandle {
    inner: Arc<Mutex<BudgetSnapshot>>,
}

impl BudgetHandle {
    pub fn new(snapshot: BudgetSnapshot) -> Self {
        Self {
            inner: Arc::new(Mutex::new(snapshot)),
        }
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        self.inner.lock().expect("budget lock poisoned").clone()
    }

    /// Merge committed consumption while preserving concurrent reservations.
    pub fn advance_to(&self, snapshot: &BudgetSnapshot) -> bool {
        let mut budget = self.inner.lock().expect("budget lock poisoned");
        if budget.max_turns != snapshot.max_turns
            || budget.max_tool_calls != snapshot.max_tool_calls
        {
            return false;
        }
        budget.turns_used = budget.turns_used.max(snapshot.turns_used);
        budget.tool_calls_used = budget.tool_calls_used.max(snapshot.tool_calls_used);
        true
    }
    pub fn remaining_tool_calls(&self) -> u32 {
        self.inner
            .lock()
            .expect("budget lock poisoned")
            .remaining_tool_calls()
    }

    /// Reserve a child scope from the shared parent budget.
    pub fn reserve_child(&self, max_turns: u32, max_tool_calls: u32) -> Option<BudgetSnapshot> {
        self.inner
            .lock()
            .expect("budget lock poisoned")
            .reserve_child(max_turns, max_tool_calls)
    }
}

impl BudgetSnapshot {
    pub fn remaining_tool_calls(&self) -> u32 {
        self.max_tool_calls.saturating_sub(self.tool_calls_used)
    }

    /// Reserve a child scope from this budget.
    ///
    /// One turn is charged to the parent, and the child's whole tool-call
    /// allowance is charged up front. Unused reservations are not returned.
    pub fn reserve_child(&mut self, max_turns: u32, max_tool_calls: u32) -> Option<BudgetSnapshot> {
        if !self.turn_available() || !self.tool_calls_available(max_tool_calls) {
            return None;
        }
        self.turns_used += 1;
        self.tool_calls_used += max_tool_calls;
        Some(BudgetSnapshot::new(max_turns, max_tool_calls))
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::task::Wake;
    struct Probe(AtomicUsize);
    impl Wake for Probe {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn probe() -> (Arc<Probe>, Waker) {
        let p = Arc::new(Probe(AtomicUsize::new(0)));
        (p.clone(), Waker::from(p))
    }
    #[test]
    fn parent_cancellation_wakes_child_without_reverse_propagation() {
        let parent = CancelFlag::new();
        let child = CancelFlag::child_of(&parent);
        let other = CancelFlag::new();
        let (p, w) = probe();
        let mut wait = Box::pin(child.cancelled());
        assert!(
            wait.as_mut()
                .poll(&mut Context::from_waker(&w))
                .is_pending()
        );
        parent.cancel();
        assert_eq!(p.0.load(Ordering::SeqCst), 1);
        assert!(wait.as_mut().poll(&mut Context::from_waker(&w)).is_ready());
        assert!(!other.is_cancelled());
        let parent = CancelFlag::new();
        let child = CancelFlag::child_of(&parent);
        child.cancel();
        assert!(!parent.is_cancelled());
    }
    #[test]
    fn replacing_and_dropping_waiters_leaves_no_stale_wakers() {
        let flag = CancelFlag::new();
        let (old, old_waker) = probe();
        let (new, new_waker) = probe();
        let mut wait = Box::pin(flag.cancelled());
        assert!(
            wait.as_mut()
                .poll(&mut Context::from_waker(&old_waker))
                .is_pending()
        );
        assert!(
            wait.as_mut()
                .poll(&mut Context::from_waker(&new_waker))
                .is_pending()
        );
        flag.cancel();
        assert_eq!(old.0.load(Ordering::SeqCst), 0);
        assert_eq!(new.0.load(Ordering::SeqCst), 1);
        drop(wait);
        let flag = CancelFlag::new();
        let mut wait = Box::pin(flag.cancelled());
        assert!(
            wait.as_mut()
                .poll(&mut Context::from_waker(&new_waker))
                .is_pending()
        );
        drop(wait);
        assert!(flag.state.waiters.lock().unwrap().is_empty());
        flag.cancel();
        assert_eq!(new.0.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn cancellation_during_poll_or_before_poll_never_stays_pending() {
        for _ in 0..64 {
            let flag = CancelFlag::new();
            let cancelling = flag.clone();
            let thread = std::thread::spawn(move || cancelling.cancel());
            let (_, waker) = probe();
            let mut wait = Box::pin(flag.cancelled());
            let _ = wait.as_mut().poll(&mut Context::from_waker(&waker));
            thread.join().unwrap();
            assert!(
                wait.as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_ready()
            );
            assert!(flag.state.waiters.lock().unwrap().is_empty());
        }
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    state: Arc<AtomicBool>,
    parent: Option<Arc<CancelFlag>>,
}

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a child scope flag that inherits parent cancellation.
    pub fn child_of(parent: &CancelFlag) -> Self {
        Self {
            state: Arc::new(AtomicBool::new(false)),
            parent: Some(Arc::new(parent.clone())),
        }
    }

    pub fn cancel(&self) {
        self.state.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::SeqCst)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
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

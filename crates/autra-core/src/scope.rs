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

/// Cooperative cancellation shared between a caller and the local host.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Permission, budget and cancellation identity for a run.
#[derive(Clone, Debug)]
pub struct ExecutionScope {
    pub run_id: RunId,
    pub parent_scope_id: Option<String>,
    pub max_turns: u32,
    pub max_tool_calls: u32,
    pub cancellation: CancelFlag,
}

impl ExecutionScope {
    pub fn new(run_id: RunId, max_turns: u32, max_tool_calls: u32) -> Self {
        Self {
            run_id,
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
}

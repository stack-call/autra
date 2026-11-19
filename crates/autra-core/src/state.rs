use serde::{Deserialize, Serialize};

use crate::ids::RunId;
use crate::message::{HistoryItem, ToolCallRequest};
use crate::scope::BudgetSnapshot;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentPhase {
    Ready,
    AwaitingModel,
    AwaitingTool,
    Completed,
    Failed,
    Cancelled,
}

/// Authoritative state of one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    pub run_id: RunId,
    pub phase: AgentPhase,
    pub history: Vec<HistoryItem>,
    pub pending_tool_calls: Vec<ToolCallRequest>,
    pub budget: BudgetSnapshot,
    pub final_answer: Option<String>,
    pub last_error: Option<String>,
}

impl AgentState {
    pub fn new(run_id: RunId, budget: BudgetSnapshot) -> Self {
        Self {
            run_id,
            phase: AgentPhase::Ready,
            history: Vec::new(),
            pending_tool_calls: Vec::new(),
            budget,
            final_answer: None,
            last_error: None,
        }
    }
}

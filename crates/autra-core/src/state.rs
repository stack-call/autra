use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{RunId, SessionId, TurnId};
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

/// State owned by a specific engine. Core stores it opaquely and versions it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineState {
    pub engine_id: String,
    pub schema_revision: u32,
    pub data: Value,
}

impl EngineState {
    pub fn new(engine_id: impl Into<String>, schema_revision: u32) -> Self {
        Self {
            engine_id: engine_id.into(),
            schema_revision,
            data: Value::Null,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
}

/// Authoritative state of one run.
///
/// Core owns identity, phase, budget, final answer, last error and an opaque,
/// versioned `engine_state`. Engine-specific facts (for example ReAct history
/// and pending tool calls) belong to the engine, not to Core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    pub run_id: RunId,
    /// Monotonic version of the committed run state. A journal uses it for
    /// optimistic concurrency checks.
    #[serde(default)]
    pub state_version: u64,
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
    pub phase: AgentPhase,
    pub budget: BudgetSnapshot,
    pub final_answer: Option<String>,
    pub last_error: Option<String>,
    pub engine_state: EngineState,
}

impl AgentState {
    pub fn new(run_id: RunId, budget: BudgetSnapshot) -> Self {
        Self {
            run_id,
            state_version: 0,
            session_id: None,
            turn_id: None,
            phase: AgentPhase::Ready,
            budget,
            final_answer: None,
            last_error: None,
            engine_state: EngineState::new("none", 0),
        }
    }

    pub fn with_engine_state(mut self, engine_state: EngineState) -> Self {
        self.engine_state = engine_state;
        self
    }
}

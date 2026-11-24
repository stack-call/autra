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
/// A JSON Patch (RFC 6902) from one engine-state version to the next.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStateDelta {
    pub base_version: u64,
    /// Patch applied to the serialized `EngineState` object.
    pub patch: serde_json::Value,
}

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
    #[serde(default)]
    pub parent_run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
    pub phase: AgentPhase,
    pub budget: BudgetSnapshot,
    pub final_answer: Option<String>,
    pub last_error: Option<String>,
    /// Stable identity of the Agent configuration that created this Run.
    /// Recovery refuses to continue a Run with a different profile.
    #[serde(default)]
    pub agent_profile: Option<String>,
    pub engine_state: EngineState,
}

impl AgentState {
    pub fn new(run_id: RunId, budget: BudgetSnapshot) -> Self {
        Self {
            run_id,
            state_version: 0,
            parent_run_id: None,
            session_id: None,
            turn_id: None,
            phase: AgentPhase::Ready,
            budget,
            final_answer: None,
            last_error: None,
            agent_profile: None,
            engine_state: EngineState::new("none", 0),
        }
    }

    pub fn with_engine_state(mut self, engine_state: EngineState) -> Self {
        self.engine_state = engine_state;
        self
    }
}

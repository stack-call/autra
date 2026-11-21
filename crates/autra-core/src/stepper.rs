use thiserror::Error;

use crate::effect::EffectIntent;
use crate::event::AcceptedEvent;
use crate::message::{HistoryItem, ToolCallRequest};
use crate::state::AgentState;

pub const STEPPER_PROTOCOL_VERSION: u32 = 1;

/// Sans-I/O state machine for one agent behaviour.
pub trait AgentStepper: Send + Sync {
    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<TransitionProposal, StepError>;

    fn protocol_version(&self) -> u32 {
        STEPPER_PROTOCOL_VERSION
    }

    /// Initialize engine-owned state when a Run starts. The engine calls this
    /// once with the seeded conversation history; a resumed Run is not
    /// re-initialized.
    fn initialize(&self, _state: &mut AgentState, _history: &[HistoryItem]) {}

    /// Tool calls the engine has accepted but not yet seen results for. A
    /// non-ReAct engine may return an empty list. Hosts must treat an error as
    /// a corrupt or unsupported engine state rather than an empty list.
    fn pending_tool_calls(&self, _state: &AgentState) -> Result<Vec<ToolCallRequest>, StepError> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug)]
pub struct TransitionProposal {
    pub next: AgentState,
    pub intents: Vec<EffectIntent>,
    pub disposition: Disposition,
}

/// Backward-compatible alias. New code should name the Proposal explicitly.
pub type Transition = TransitionProposal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Continue,
    Finished,
    Failed,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct StepError {
    pub message: String,
}

impl StepError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

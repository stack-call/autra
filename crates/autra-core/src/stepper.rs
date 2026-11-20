use thiserror::Error;

use crate::effect::EffectIntent;
use crate::event::AcceptedEvent;
use crate::state::AgentState;

pub const STEPPER_PROTOCOL_VERSION: u32 = 1;

/// Sans-I/O state machine for one agent behaviour.
pub trait AgentStepper: Send + Sync {
    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<Transition, StepError>;

    fn protocol_version(&self) -> u32 {
        STEPPER_PROTOCOL_VERSION
    }
}

#[derive(Clone, Debug)]
pub struct Transition {
    pub next: AgentState,
    pub intents: Vec<EffectIntent>,
    pub disposition: Disposition,
}

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

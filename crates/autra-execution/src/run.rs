//! Public run lifecycle and event types.

use std::sync::Arc;
use std::time::Duration;

use autra_core::{
    AgentPhase, AgentState, CancelFlag, EffectRecord, RunId, SessionId, StepError, ToolCallRequest,
    TurnId,
};
use thiserror::Error;

use crate::approval::Approver;
use crate::model::ModelError;

#[derive(Debug, Error)]
pub enum RunError {
    #[error("step error: {0}")]
    Step(#[from] StepError),
    #[error("model error: {0}")]
    Model(#[from] ModelError),
    #[error("run failed: {0}")]
    Failed(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("journal error: {0}")]
    Journal(String),
    #[error("session store error: {0}")]
    Session(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Completed,
    Cancelled,
}

#[derive(Clone, Default)]
pub struct RunOptions {
    pub cancel: CancelFlag,
    pub effect_timeout: Option<Duration>,
    pub max_parallel_tool_calls: Option<u32>,
    pub approver: Option<Arc<dyn Approver>>,
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
}

#[derive(Clone, Debug)]
pub enum RunEvent {
    RunStarted {
        run_id: RunId,
    },
    RunResumed {
        run_id: RunId,
    },
    ModelCallStarted {
        turn: u32,
    },
    ModelResult {
        text: Option<String>,
        tool_calls: Vec<ToolCallRequest>,
    },
    ApprovalRequested {
        call_id: String,
        tool_name: String,
        argument_digest: String,
    },
    ApprovalDecided {
        call_id: String,
        approved: bool,
    },
    ToolCallStarted {
        call_id: String,
        name: String,
    },
    ToolResult {
        call_id: String,
        name: String,
        is_error: bool,
        outcome_unknown: bool,
        content: String,
    },
    RunCompleted {
        answer: Option<String>,
    },
    RunCancelled,
    RunFailed {
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct RunOutcome {
    state: AgentState,
    effects: Vec<EffectRecord>,
}

impl RunOutcome {
    pub fn new(state: AgentState, effects: Vec<EffectRecord>) -> Self {
        Self { state, effects }
    }

    pub fn state(&self) -> &AgentState {
        &self.state
    }

    pub fn effects(&self) -> &[EffectRecord] {
        &self.effects
    }

    pub fn answer(&self) -> Option<&str> {
        self.state.final_answer.as_deref()
    }

    pub fn status(&self) -> RunStatus {
        match self.state.phase {
            AgentPhase::Completed => RunStatus::Completed,
            AgentPhase::Cancelled => RunStatus::Cancelled,
            ref phase => panic!("RunOutcome only exists for Completed or Cancelled, got {phase:?}"),
        }
    }

    pub fn turns_used(&self) -> u32 {
        self.state.budget.turns_used
    }

    pub fn tool_calls_used(&self) -> u32 {
        self.state.budget.tool_calls_used
    }
}

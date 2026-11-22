//! Validation of a stepper proposal before any Effect is recorded or dispatched.
//!
//! The stepper owns its behaviour, but it does not own Run identity, budget
//! limits, or the legal phase graph. These invariants are enforced centrally so
//! a custom engine cannot silently widen its authority.

use autra_core::{AgentPhase, AgentState};
use autra_execution::RunError;

pub(super) fn validate_initialized(
    current: &AgentState,
    next: &AgentState,
) -> Result<(), RunError> {
    if next.run_id != current.run_id {
        return Err(invalid("initialize must not change the run id"));
    }
    if next.parent_run_id != current.parent_run_id {
        return Err(invalid("initialize must not change the parent run id"));
    }
    if next.session_id != current.session_id || next.turn_id != current.turn_id {
        return Err(invalid(
            "initialize must not change session or turn identity",
        ));
    }
    if next.state_version != current.state_version {
        return Err(invalid("initialize must not change state_version"));
    }
    if next.budget != current.budget {
        return Err(invalid("initialize must not change the budget"));
    }
    if next.phase != AgentPhase::Ready {
        return Err(invalid("initialize must leave the run in Ready"));
    }
    Ok(())
}

pub(super) fn validate(
    current: &AgentState,
    next: &AgentState,
    expected_version: u64,
) -> Result<(), RunError> {
    if next.run_id != current.run_id {
        return Err(invalid("a transition must not change the run id"));
    }
    if next.parent_run_id != current.parent_run_id {
        return Err(invalid("a transition must not change the parent run id"));
    }
    if next.session_id != current.session_id || next.turn_id != current.turn_id {
        return Err(invalid(
            "a transition must not change session or turn identity",
        ));
    }
    if next.state_version != expected_version + 1 {
        return Err(invalid(format!(
            "a transition must advance state_version from {expected_version} to {}, got {}",
            expected_version + 1,
            next.state_version
        )));
    }
    if next.budget.max_turns != current.budget.max_turns
        || next.budget.max_tool_calls != current.budget.max_tool_calls
    {
        return Err(invalid("a transition must not change budget limits"));
    }
    if next.budget.turns_used < current.budget.turns_used {
        return Err(invalid("turns_used must not decrease"));
    }
    if next.budget.tool_calls_used < current.budget.tool_calls_used {
        return Err(invalid("tool_calls_used must not decrease"));
    }
    if next.budget.turns_used > next.budget.max_turns {
        return Err(invalid("turns_used exceeds max_turns"));
    }
    if next.budget.tool_calls_used > next.budget.max_tool_calls {
        return Err(invalid("tool_calls_used exceeds max_tool_calls"));
    }
    if current.engine_state.engine_id != "none"
        && next.engine_state.engine_id != current.engine_state.engine_id
    {
        return Err(invalid("a transition must not switch engines mid-run"));
    }
    if !legal_phase(&current.phase, &next.phase) {
        return Err(invalid(format!(
            "illegal phase transition {:?} -> {:?}",
            current.phase, next.phase
        )));
    }
    if next.phase == AgentPhase::Failed && next.last_error.is_none() {
        return Err(invalid("a failed state must record last_error"));
    }
    Ok(())
}

fn legal_phase(current: &AgentPhase, next: &AgentPhase) -> bool {
    use AgentPhase::{AwaitingModel, AwaitingTool, Cancelled, Completed, Failed, Ready};
    match current {
        Ready => matches!(
            next,
            AwaitingModel | AwaitingTool | Completed | Failed | Cancelled
        ),
        AwaitingModel => matches!(
            next,
            AwaitingModel | AwaitingTool | Completed | Failed | Cancelled
        ),
        AwaitingTool => matches!(
            next,
            AwaitingTool | AwaitingModel | Completed | Failed | Cancelled
        ),
        Completed | Failed | Cancelled => next == current,
    }
}

fn invalid(message: impl Into<String>) -> RunError {
    RunError::Protocol(format!("invalid transition: {}", message.into()))
}

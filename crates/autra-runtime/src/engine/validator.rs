//! Validation of a stepper proposal before any Effect is recorded or dispatched.
//!
//! The stepper owns its behaviour, but it does not own Run identity, budget
//! limits, or the legal phase graph. These invariants are enforced centrally so
//! a custom engine cannot silently widen its authority.

use std::collections::HashSet;

use autra_core::{AcceptedEvent, AgentPhase, AgentState, Disposition, EffectIntent, EventKind};
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
    if next.agent_profile != current.agent_profile {
        return Err(invalid("initialize must not change the agent profile"));
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
    event: &AcceptedEvent,
    disposition: Disposition,
    intents: &[EffectIntent],
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
    if next.agent_profile != current.agent_profile {
        return Err(invalid("a transition must not change the agent profile"));
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
    if !legal_event(&current.phase, &event.kind) {
        return Err(invalid(format!(
            "event {:?} is not valid in phase {:?}",
            event.kind, current.phase
        )));
    }
    validate_disposition(next, disposition)?;
    validate_intents(current, next, event, intents)?;
    if next.phase == AgentPhase::Failed && next.last_error.is_none() {
        return Err(invalid("a failed state must record last_error"));
    }
    Ok(())
}

fn legal_event(phase: &AgentPhase, event: &EventKind) -> bool {
    match event {
        EventKind::RunStarted { .. } => *phase == AgentPhase::Ready,
        EventKind::ModelResult(_) => *phase == AgentPhase::AwaitingModel,
        EventKind::ToolResult(_) => *phase == AgentPhase::AwaitingTool,
        EventKind::ResumeRequested => *phase == AgentPhase::AwaitingModel,
        EventKind::CancelRequested => !matches!(
            phase,
            AgentPhase::Completed | AgentPhase::Failed | AgentPhase::Cancelled
        ),
    }
}

fn validate_disposition(next: &AgentState, disposition: Disposition) -> Result<(), RunError> {
    let consistent = match disposition {
        Disposition::Finished => {
            matches!(next.phase, AgentPhase::Completed | AgentPhase::Cancelled)
        }
        Disposition::Failed => next.phase == AgentPhase::Failed,
        Disposition::Continue => !matches!(
            next.phase,
            AgentPhase::Completed | AgentPhase::Failed | AgentPhase::Cancelled
        ),
    };
    if consistent {
        Ok(())
    } else {
        Err(invalid(format!(
            "disposition {disposition:?} is inconsistent with phase {:?}",
            next.phase
        )))
    }
}

fn validate_intents(
    current: &AgentState,
    next: &AgentState,
    event: &AcceptedEvent,
    intents: &[EffectIntent],
) -> Result<(), RunError> {
    let mut model_intents = 0;
    let mut tool_intents = 0;
    let mut call_ids = HashSet::new();
    for intent in intents {
        match intent {
            EffectIntent::CallModel(_) => model_intents += 1,
            EffectIntent::CallTool(spec) => {
                tool_intents += 1;
                if spec.call.call_id.is_empty() {
                    return Err(invalid("a tool intent must have a non-empty call id"));
                }
                if !call_ids.insert(spec.call.call_id.as_str()) {
                    return Err(invalid(format!(
                        "duplicate tool call id `{}` in one transition",
                        spec.call.call_id
                    )));
                }
            }
        }
    }
    if model_intents > 1 {
        return Err(invalid("one transition may propose at most one model call"));
    }
    if model_intents == 1 && next.phase != AgentPhase::AwaitingModel {
        return Err(invalid("a model intent requires AwaitingModel"));
    }
    if tool_intents > 0 && next.phase != AgentPhase::AwaitingTool {
        return Err(invalid("tool intents require AwaitingTool"));
    }
    if matches!(event.kind, EventKind::CancelRequested) && !intents.is_empty() {
        return Err(invalid(
            "a cancelled transition must not dispatch new intents",
        ));
    }
    let charged = next
        .budget
        .tool_calls_used
        .saturating_sub(current.budget.tool_calls_used);
    if charged != tool_intents as u32 {
        return Err(invalid(format!(
            "transition proposes {tool_intents} tool call(s) but charges {charged} to the budget"
        )));
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

#[cfg(test)]
mod tests {
    use super::validate;
    use autra_core::{
        AcceptedEvent, AgentPhase, AgentState, BudgetSnapshot, Disposition, EffectIntent,
        EventKind, ModelCallSpec, RunId, ToolCallRequest, ToolCallSpec,
    };

    fn state(phase: AgentPhase, tool_calls_used: u32) -> AgentState {
        let mut state = AgentState::new(RunId::new("run"), BudgetSnapshot::new(4, 4));
        state.phase = phase;
        state.budget.tool_calls_used = tool_calls_used;
        state
    }

    fn event(kind: EventKind) -> AcceptedEvent {
        AcceptedEvent { seq: 1, kind }
    }

    fn tool_call(call_id: &str) -> EffectIntent {
        EffectIntent::CallTool(ToolCallSpec {
            call: ToolCallRequest {
                call_id: call_id.to_owned(),
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            },
        })
    }

    fn model_call() -> EffectIntent {
        EffectIntent::CallModel(ModelCallSpec {
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
        })
    }

    fn check(
        current: &AgentState,
        next: &AgentState,
        event: &AcceptedEvent,
        disposition: Disposition,
        intents: &[EffectIntent],
    ) -> String {
        validate(current, next, 0, event, disposition, intents)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn an_event_must_match_the_current_phase() {
        let current = state(AgentPhase::AwaitingTool, 0);
        let mut next = current.clone();
        next.state_version = 1;
        let event = event(EventKind::ModelResult(autra_core::ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }));

        let error = check(&current, &next, &event, Disposition::Continue, &[]);

        assert!(error.contains("is not valid in phase"));
    }

    #[test]
    fn finished_must_end_in_a_terminal_phase() {
        let current = state(AgentPhase::Ready, 0);
        let mut next = current.clone();
        next.phase = AgentPhase::AwaitingModel;
        next.state_version = 1;

        let error = check(
            &current,
            &next,
            &event(EventKind::RunStarted {
                input: "go".to_owned(),
            }),
            Disposition::Finished,
            &[],
        );

        assert!(error.contains("inconsistent with phase"));
    }

    #[test]
    fn tool_intents_must_be_charged_to_the_budget() {
        let current = state(AgentPhase::Ready, 0);
        let mut next = current.clone();
        next.phase = AgentPhase::AwaitingTool;
        next.state_version = 1;

        let error = check(
            &current,
            &next,
            &event(EventKind::RunStarted {
                input: "go".to_owned(),
            }),
            Disposition::Continue,
            &[tool_call("call-1"), tool_call("call-2")],
        );

        assert!(error.contains("charges"));
    }

    #[test]
    fn duplicate_tool_call_ids_are_rejected() {
        let current = state(AgentPhase::Ready, 0);
        let mut next = current.clone();
        next.phase = AgentPhase::AwaitingTool;
        next.budget.tool_calls_used = 2;
        next.state_version = 1;

        let error = check(
            &current,
            &next,
            &event(EventKind::RunStarted {
                input: "go".to_owned(),
            }),
            Disposition::Continue,
            &[tool_call("call-1"), tool_call("call-1")],
        );

        assert!(error.contains("duplicate tool call id"));
    }

    #[test]
    fn a_model_intent_requires_awaiting_model() {
        let current = state(AgentPhase::Ready, 0);
        let mut next = current.clone();
        next.phase = AgentPhase::AwaitingTool;
        next.state_version = 1;

        let error = check(
            &current,
            &next,
            &event(EventKind::RunStarted {
                input: "go".to_owned(),
            }),
            Disposition::Continue,
            &[model_call()],
        );

        assert!(error.contains("requires AwaitingModel"));
    }
}

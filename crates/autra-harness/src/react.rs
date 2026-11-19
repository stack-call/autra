use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, Disposition, EffectIntent, EventKind,
    HistoryItem, ModelCallSpec, ModelOutput, StepError, ToolSpec, Transition,
};

/// The default ReAct engine: model call, then final answer.
pub struct ReActStepper {
    instructions: Option<String>,
    tools: Vec<ToolSpec>,
}

impl ReActStepper {
    pub fn new(instructions: Option<String>, tools: Vec<ToolSpec>) -> Self {
        Self {
            instructions,
            tools,
        }
    }

    fn model_request(&self, state: &AgentState) -> ModelCallSpec {
        ModelCallSpec {
            system: self.instructions.clone(),
            messages: state.history.clone(),
            tools: self.tools.clone(),
        }
    }

    fn failed(mut state: AgentState, message: impl Into<String>) -> Transition {
        state.phase = AgentPhase::Failed;
        state.last_error = Some(message.into());
        Transition {
            next: state,
            intents: Vec::new(),
            disposition: Disposition::Failed,
        }
    }

    fn expect_phase(
        state: &AgentState,
        expected: AgentPhase,
        event: &str,
    ) -> Result<(), StepError> {
        if state.phase == expected {
            Ok(())
        } else {
            Err(StepError::new(format!(
                "unexpected {event} in phase {:?}",
                state.phase
            )))
        }
    }

    fn on_model_result(
        &self,
        state: &AgentState,
        output: &ModelOutput,
    ) -> Result<Transition, StepError> {
        Self::expect_phase(state, AgentPhase::AwaitingModel, "ModelResult")?;
        let mut next = state.clone();
        next.history.push(HistoryItem::Assistant {
            text: output.text.clone(),
            tool_calls: output.tool_calls.clone(),
        });
        if output.tool_calls.is_empty() {
            next.phase = AgentPhase::Completed;
            next.final_answer = output.text.clone();
            return Ok(Transition {
                next,
                intents: Vec::new(),
                disposition: Disposition::Finished,
            });
        }
        Ok(Self::failed(
            next,
            "tool calls are not supported in this slice",
        ))
    }
}

impl AgentStepper for ReActStepper {
    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<Transition, StepError> {
        match &event.kind {
            EventKind::RunStarted { input } => {
                Self::expect_phase(state, AgentPhase::Ready, "RunStarted")?;
                let mut next = state.clone();
                next.history.push(HistoryItem::User(input.clone()));
                if !next.budget.turn_available() {
                    return Ok(Self::failed(next, "max turns exceeded"));
                }
                next.budget.turns_used += 1;
                next.phase = AgentPhase::AwaitingModel;
                let request = self.model_request(&next);
                Ok(Transition {
                    next,
                    intents: vec![EffectIntent::CallModel(request)],
                    disposition: Disposition::Continue,
                })
            }
            EventKind::ModelResult(output) => self.on_model_result(state, output),
            EventKind::ToolResult(_) => Err(StepError::new(
                "tool results are not supported in this slice",
            )),
            EventKind::CancelRequested => {
                if matches!(
                    state.phase,
                    AgentPhase::Completed | AgentPhase::Failed | AgentPhase::Cancelled
                ) {
                    return Err(StepError::new(format!(
                        "unexpected CancelRequested in phase {:?}",
                        state.phase
                    )));
                }
                let mut next = state.clone();
                next.phase = AgentPhase::Cancelled;
                Ok(Transition {
                    next,
                    intents: Vec::new(),
                    disposition: Disposition::Finished,
                })
            }
        }
    }
}

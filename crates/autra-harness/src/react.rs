use std::collections::HashSet;
use std::sync::Arc;

use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, Disposition, EffectIntent, EngineState,
    EventKind, HistoryItem, ModelCallSpec, ModelOutput, StepError, ToolCallRequest, ToolCallSpec,
    ToolOutcome, ToolSpec, Transition,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::{ContextBuilder, ContextRequest, SimpleContextBuilder};

const REACT_ENGINE_ID: &str = "react";
const REACT_SCHEMA_REVISION: u32 = 1;

/// State owned by the ReAct engine and stored opaquely in `AgentState`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ReactState {
    #[serde(default)]
    history: Vec<HistoryItem>,
    #[serde(default)]
    pending_tool_calls: Vec<ToolCallRequest>,
}

fn react_state(state: &AgentState) -> ReactState {
    if state.engine_state.engine_id != REACT_ENGINE_ID {
        return ReactState::default();
    }
    serde_json::from_value(state.engine_state.data.clone()).unwrap_or_default()
}

fn store_react_state(next: &mut AgentState, react: ReactState) {
    next.engine_state = EngineState::new(REACT_ENGINE_ID, REACT_SCHEMA_REVISION)
        .with_data(serde_json::to_value(react).unwrap_or(Value::Null));
}

/// The conversation history of a ReAct run.
pub fn react_history(state: &AgentState) -> Vec<HistoryItem> {
    react_state(state).history
}

/// Tool calls that ReAct has not yet received results for.
pub fn react_pending_tool_calls(state: &AgentState) -> Vec<ToolCallRequest> {
    react_state(state).pending_tool_calls
}

/// Builds an EngineState for a ReAct checkpoint. Useful for resume fixtures.
pub fn react_engine_state(
    history: Vec<HistoryItem>,
    pending_tool_calls: Vec<ToolCallRequest>,
) -> EngineState {
    let data = serde_json::to_value(ReactState {
        history,
        pending_tool_calls,
    })
    .unwrap_or(Value::Null);
    EngineState::new(REACT_ENGINE_ID, REACT_SCHEMA_REVISION).with_data(data)
}

/// The default ReAct engine: model call, tool batch, then final answer.
pub struct ReActStepper {
    instructions: Option<String>,
    tools: Vec<ToolSpec>,
    context_builder: Arc<dyn ContextBuilder>,
}

impl ReActStepper {
    pub fn new(instructions: Option<String>, tools: Vec<ToolSpec>) -> Self {
        Self {
            instructions,
            tools,
            context_builder: Arc::new(SimpleContextBuilder::default()),
        }
    }

    pub fn with_context_builder(mut self, builder: impl ContextBuilder + 'static) -> Self {
        self.context_builder = Arc::new(builder);
        self
    }

    pub fn with_shared_context_builder(mut self, builder: Arc<dyn ContextBuilder>) -> Self {
        self.context_builder = builder;
        self
    }

    fn model_request(&self, react: &ReactState) -> Result<ModelCallSpec, StepError> {
        self.context_builder
            .build(ContextRequest {
                instructions: self.instructions.as_deref(),
                history: &react.history,
                tools: &self.tools,
            })
            .map_err(|error| StepError::new(error.to_string()))
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
        let mut react = react_state(state);
        react.history.push(HistoryItem::Assistant {
            text: output.text.clone(),
            tool_calls: output.tool_calls.clone(),
        });
        let mut next = state.clone();
        store_react_state(&mut next, react.clone());

        if output.tool_calls.is_empty() {
            next.phase = AgentPhase::Completed;
            next.final_answer = output.text.clone();
            return Ok(Transition {
                next,
                intents: Vec::new(),
                disposition: Disposition::Finished,
            });
        }

        let mut seen = HashSet::new();
        for call in &output.tool_calls {
            if call.call_id.is_empty() {
                return Ok(Self::failed(next, "tool call id must not be empty"));
            }
            if !seen.insert(call.call_id.as_str()) {
                return Ok(Self::failed(
                    next,
                    format!("duplicate tool call id `{}`", call.call_id),
                ));
            }
        }

        let requested = output.tool_calls.len() as u32;
        if !next.budget.tool_calls_available(requested) {
            let remaining = next.budget.remaining_tool_calls();
            return Ok(Self::failed(
                next,
                format!("max tool calls exceeded: {requested} requested, {remaining} remaining"),
            ));
        }
        next.budget.tool_calls_used += requested;
        react.pending_tool_calls = output.tool_calls.clone();
        store_react_state(&mut next, react);
        next.phase = AgentPhase::AwaitingTool;

        let intents = output
            .tool_calls
            .iter()
            .cloned()
            .map(|call| EffectIntent::CallTool(ToolCallSpec { call }))
            .collect();
        Ok(Transition {
            next,
            intents,
            disposition: Disposition::Continue,
        })
    }

    fn on_tool_result(
        &self,
        state: &AgentState,
        outcome: &ToolOutcome,
    ) -> Result<Transition, StepError> {
        Self::expect_phase(state, AgentPhase::AwaitingTool, "ToolResult")?;
        let mut react = react_state(state);
        if !react
            .pending_tool_calls
            .iter()
            .any(|call| call.call_id == outcome.call_id)
        {
            return Ok(Self::failed(
                state.clone(),
                format!("tool result for unknown call id `{}`", outcome.call_id),
            ));
        }

        react.history.push(HistoryItem::Tool(outcome.clone()));
        react
            .pending_tool_calls
            .retain(|call| call.call_id != outcome.call_id);
        let mut next = state.clone();
        store_react_state(&mut next, react.clone());

        if !react.pending_tool_calls.is_empty() {
            return Ok(Transition {
                next,
                intents: Vec::new(),
                disposition: Disposition::Continue,
            });
        }

        if !next.budget.turn_available() {
            return Ok(Self::failed(next, "max turns exceeded"));
        }
        next.budget.turns_used += 1;
        next.phase = AgentPhase::AwaitingModel;
        let request = self.model_request(&react)?;
        Ok(Transition {
            next,
            intents: vec![EffectIntent::CallModel(request)],
            disposition: Disposition::Continue,
        })
    }
}

impl AgentStepper for ReActStepper {
    fn pending_tool_calls(&self, state: &AgentState) -> Vec<ToolCallRequest> {
        react_state(state).pending_tool_calls
    }

    fn initialize(&self, state: &mut AgentState, history: &[HistoryItem]) {
        store_react_state(
            state,
            ReactState {
                history: history.to_vec(),
                pending_tool_calls: Vec::new(),
            },
        );
    }

    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<Transition, StepError> {
        match &event.kind {
            EventKind::RunStarted { input } => {
                Self::expect_phase(state, AgentPhase::Ready, "RunStarted")?;
                let mut react = react_state(state);
                react.history.push(HistoryItem::User(input.clone()));
                let mut next = state.clone();
                store_react_state(&mut next, react.clone());
                if !next.budget.turn_available() {
                    return Ok(Self::failed(next, "max turns exceeded"));
                }
                next.budget.turns_used += 1;
                next.phase = AgentPhase::AwaitingModel;
                let request = self.model_request(&react)?;
                Ok(Transition {
                    next,
                    intents: vec![EffectIntent::CallModel(request)],
                    disposition: Disposition::Continue,
                })
            }
            EventKind::ModelResult(output) => self.on_model_result(state, output),
            EventKind::ToolResult(outcome) => self.on_tool_result(state, outcome),
            EventKind::ResumeRequested => {
                Self::expect_phase(state, AgentPhase::AwaitingModel, "ResumeRequested")?;
                if !state.budget.turn_available() {
                    return Ok(Self::failed(state.clone(), "max turns exceeded"));
                }
                let mut next = state.clone();
                next.budget.turns_used += 1;
                let request = self.model_request(&react_state(state))?;
                Ok(Transition {
                    next,
                    intents: vec![EffectIntent::CallModel(request)],
                    disposition: Disposition::Continue,
                })
            }
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

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, Disposition, EffectIntent, EngineState,
    EngineStateDelta, EventKind, HistoryItem, ModelCallSpec, ModelOutput, RunId, StepError,
    ToolCallRequest, ToolCallSpec, ToolOutcome, ToolSpec, Transition,
};
use serde::{Deserialize, Serialize};

use crate::context::{ContextBuilder, ContextRequest, SimpleContextBuilder};

const REACT_ENGINE_ID: &str = "react";
const REACT_SCHEMA_REVISION: u32 = 1;
const UNINITIALIZED_ENGINE_ID: &str = "none";

/// State owned by the ReAct engine and stored opaquely in `AgentState`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ReactState {
    #[serde(default)]
    history: Vec<HistoryItem>,
    #[serde(default)]
    pending_tool_calls: Vec<ToolCallRequest>,
}

fn try_react_state(state: &AgentState) -> Result<ReactState, StepError> {
    let engine = &state.engine_state;
    if engine.engine_id == UNINITIALIZED_ENGINE_ID {
        return Ok(ReactState::default());
    }
    if engine.engine_id != REACT_ENGINE_ID {
        return Err(StepError::new(format!(
            "engine state belongs to `{}`, not `{REACT_ENGINE_ID}`",
            engine.engine_id
        )));
    }
    match engine.schema_revision {
        REACT_SCHEMA_REVISION => serde_json::from_value(engine.data.clone()).map_err(|error| {
            StepError::new(format!(
                "cannot decode react engine state revision {REACT_SCHEMA_REVISION}: {error}"
            ))
        }),
        revision if revision > REACT_SCHEMA_REVISION => Err(StepError::new(format!(
            "react engine state revision {revision} is newer than supported {REACT_SCHEMA_REVISION}"
        ))),
        revision => Err(StepError::new(format!(
            "react engine state revision {revision} needs a migration that is not implemented"
        ))),
    }
}

fn store_react_state(next: &mut AgentState, react: &ReactState) {
    let data = serde_json::to_value(react).expect("react state must be serializable");
    next.engine_state = EngineState::new(REACT_ENGINE_ID, REACT_SCHEMA_REVISION).with_data(data);
}

/// Build the next state without cloning the previous engine payload. Every
/// ReAct transition replaces the engine payload, so copying it first is wasted
/// work that grows with the conversation history.
fn react_delta(base_version: u64, change: ReactChange) -> Option<EngineStateDelta> {
    let patch = match change {
        ReactChange::History(item) => {
            let item = serde_json::to_value(item).ok()?;
            serde_json::json!([
                { "op": "add", "path": "/data/history/-", "value": item }
            ])
        }
        ReactChange::HistoryAndPending { item, pending } => {
            let item = serde_json::to_value(item).ok()?;
            let pending = serde_json::to_value(pending).ok()?;
            serde_json::json!([
                { "op": "add", "path": "/data/history/-", "value": item },
                { "op": "replace", "path": "/data/pending_tool_calls", "value": pending }
            ])
        }
    };
    Some(EngineStateDelta {
        base_version,
        patch,
    })
}

fn fork_state(state: &AgentState) -> AgentState {
    AgentState {
        run_id: state.run_id.clone(),
        state_version: state.state_version,
        parent_run_id: state.parent_run_id.clone(),
        session_id: state.session_id.clone(),
        turn_id: state.turn_id.clone(),
        phase: state.phase.clone(),
        budget: state.budget.clone(),
        final_answer: state.final_answer.clone(),
        last_error: state.last_error.clone(),
        agent_profile: state.agent_profile.clone(),
        engine_state: EngineState::new(UNINITIALIZED_ENGINE_ID, 0),
    }
}

/// The conversation history of a ReAct run.
pub fn react_history(state: &AgentState) -> Result<Vec<HistoryItem>, StepError> {
    Ok(try_react_state(state)?.history)
}

/// Tool calls that ReAct has not yet received results for.
pub fn react_pending_tool_calls(state: &AgentState) -> Result<Vec<ToolCallRequest>, StepError> {
    Ok(try_react_state(state)?.pending_tool_calls)
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
    .expect("react state must be serializable");
    EngineState::new(REACT_ENGINE_ID, REACT_SCHEMA_REVISION).with_data(data)
}

enum ReactChange {
    History(HistoryItem),
    HistoryAndPending {
        item: HistoryItem,
        pending: Vec<ToolCallRequest>,
    },
}

/// The default ReAct engine: model call, tool batch, then final answer.
pub struct ReActStepper {
    instructions: Option<String>,
    tools: Vec<ToolSpec>,
    context_builder: Arc<dyn ContextBuilder>,
    /// The last decoded state, keyed by the version it belongs to. Taking it
    /// avoids deserializing the full history on every transition.
    cached: Mutex<Option<CachedReact>>,
}

struct CachedReact {
    run_id: RunId,
    state_version: u64,
    state: ReactState,
}

impl ReActStepper {
    pub fn new(instructions: Option<String>, tools: Vec<ToolSpec>) -> Self {
        Self {
            instructions,
            tools,
            context_builder: Arc::new(SimpleContextBuilder::default()),
            cached: Mutex::new(None),
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

    fn take_react_state(&self, state: &AgentState) -> Result<ReactState, StepError> {
        let mut cached = self.cached.lock().expect("react cache poisoned");
        if cached.as_ref().is_some_and(|entry| {
            entry.run_id == state.run_id && entry.state_version == state.state_version
        }) {
            return Ok(cached.take().expect("checked").state);
        }
        drop(cached);
        try_react_state(state)
    }

    fn cache(&self, next: &AgentState, react: ReactState, state_version: u64) {
        *self.cached.lock().expect("react cache poisoned") = Some(CachedReact {
            run_id: next.run_id.clone(),
            state_version,
            state: react,
        });
    }

    /// Serialize the new state once, then keep the same value for the next
    /// transition. The engine advances `state_version` by one after the
    /// proposal, so the cache is keyed to that next version.
    fn store_transition(
        &self,
        next: &mut AgentState,
        react: ReactState,
        change: ReactChange,
    ) -> Option<EngineStateDelta> {
        let delta = react_delta(next.state_version, change);
        store_react_state(next, &react);
        self.cache(next, react, next.state_version + 1);
        delta
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
            engine_delta: None,
        }
    }

    fn failed_with_delta(
        mut state: AgentState,
        message: impl Into<String>,
        delta: Option<EngineStateDelta>,
    ) -> Transition {
        state.phase = AgentPhase::Failed;
        state.last_error = Some(message.into());
        Transition {
            next: state,
            intents: Vec::new(),
            disposition: Disposition::Failed,
            engine_delta: delta,
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
        let mut react = self.take_react_state(state)?;
        let assistant = HistoryItem::Assistant {
            text: output.text.clone(),
            tool_calls: output.tool_calls.clone(),
        };
        react.history.push(assistant.clone());
        let mut next = fork_state(state);

        if output.tool_calls.is_empty() {
            next.phase = AgentPhase::Completed;
            next.final_answer = output.text.clone();
            let delta = self.store_transition(&mut next, react, ReactChange::History(assistant));
            return Ok(Transition {
                next,
                intents: Vec::new(),
                disposition: Disposition::Finished,
                engine_delta: delta,
            });
        }

        let mut seen = HashSet::new();
        for call in &output.tool_calls {
            if call.call_id.is_empty() {
                let delta = self.store_transition(
                    &mut next,
                    react,
                    ReactChange::History(assistant.clone()),
                );
                return Ok(Self::failed_with_delta(
                    next,
                    "tool call id must not be empty",
                    delta,
                ));
            }
            if !seen.insert(call.call_id.as_str()) {
                let delta = self.store_transition(
                    &mut next,
                    react,
                    ReactChange::History(assistant.clone()),
                );
                return Ok(Self::failed_with_delta(
                    next,
                    format!("duplicate tool call id `{}`", call.call_id),
                    delta,
                ));
            }
        }

        let requested = output.tool_calls.len() as u32;
        if !next.budget.tool_calls_available(requested) {
            let remaining = next.budget.remaining_tool_calls();
            let delta =
                self.store_transition(&mut next, react, ReactChange::History(assistant.clone()));
            return Ok(Self::failed_with_delta(
                next,
                format!("max tool calls exceeded: {requested} requested, {remaining} remaining"),
                delta,
            ));
        }
        next.budget.tool_calls_used += requested;
        react.pending_tool_calls = output.tool_calls.clone();
        let delta = self.store_transition(
            &mut next,
            react,
            ReactChange::HistoryAndPending {
                item: assistant,
                pending: output.tool_calls.clone(),
            },
        );
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
            engine_delta: delta,
        })
    }

    fn on_tool_result(
        &self,
        state: &AgentState,
        outcome: &ToolOutcome,
    ) -> Result<Transition, StepError> {
        Self::expect_phase(state, AgentPhase::AwaitingTool, "ToolResult")?;
        let mut react = self.take_react_state(state)?;
        if !react
            .pending_tool_calls
            .iter()
            .any(|call| call.call_id == outcome.call_id)
        {
            self.cache(state, react, state.state_version);
            return Ok(Self::failed(
                state.clone(),
                format!("tool result for unknown call id `{}`", outcome.call_id),
            ));
        }

        let tool_item = HistoryItem::Tool(outcome.clone());
        react.history.push(tool_item.clone());
        react
            .pending_tool_calls
            .retain(|call| call.call_id != outcome.call_id);
        let mut next = fork_state(state);

        if !react.pending_tool_calls.is_empty() {
            let pending = react.pending_tool_calls.clone();
            let delta = self.store_transition(
                &mut next,
                react,
                ReactChange::HistoryAndPending {
                    item: tool_item,
                    pending,
                },
            );
            return Ok(Transition {
                next,
                intents: Vec::new(),
                disposition: Disposition::Continue,
                engine_delta: delta,
            });
        }

        if !next.budget.turn_available() {
            let pending = react.pending_tool_calls.clone();
            let delta = self.store_transition(
                &mut next,
                react,
                ReactChange::HistoryAndPending {
                    item: tool_item,
                    pending,
                },
            );
            return Ok(Self::failed_with_delta(next, "max turns exceeded", delta));
        }
        next.budget.turns_used += 1;
        next.phase = AgentPhase::AwaitingModel;
        let request = self.model_request(&react)?;
        let pending = react.pending_tool_calls.clone();
        let delta = self.store_transition(
            &mut next,
            react,
            ReactChange::HistoryAndPending {
                item: tool_item,
                pending,
            },
        );
        Ok(Transition {
            next,
            intents: vec![EffectIntent::CallModel(request)],
            disposition: Disposition::Continue,
            engine_delta: delta,
        })
    }
}

impl AgentStepper for ReActStepper {
    fn pending_tool_calls(&self, state: &AgentState) -> Result<Vec<ToolCallRequest>, StepError> {
        react_pending_tool_calls(state)
    }

    fn initialize(&self, state: &mut AgentState, history: &[HistoryItem]) {
        let react = ReactState {
            history: history.to_vec(),
            pending_tool_calls: Vec::new(),
        };
        store_react_state(state, &react);
        let version = state.state_version;
        self.cache(state, react, version);
    }

    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<Transition, StepError> {
        match &event.kind {
            EventKind::RunStarted { input } => {
                Self::expect_phase(state, AgentPhase::Ready, "RunStarted")?;
                let mut react = self.take_react_state(state)?;
                let user = HistoryItem::User(input.clone());
                react.history.push(user.clone());
                let mut next = fork_state(state);
                if !next.budget.turn_available() {
                    let delta = self.store_transition(&mut next, react, ReactChange::History(user));
                    return Ok(Self::failed_with_delta(next, "max turns exceeded", delta));
                }
                next.budget.turns_used += 1;
                next.phase = AgentPhase::AwaitingModel;
                let request = self.model_request(&react)?;
                let delta = self.store_transition(&mut next, react, ReactChange::History(user));
                Ok(Transition {
                    next,
                    intents: vec![EffectIntent::CallModel(request)],
                    disposition: Disposition::Continue,
                    engine_delta: delta,
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
                let request = self.model_request(&try_react_state(state)?)?;
                Ok(Transition {
                    next,
                    intents: vec![EffectIntent::CallModel(request)],
                    disposition: Disposition::Continue,
                    engine_delta: None,
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
                    engine_delta: None,
                })
            }
        }
    }
}

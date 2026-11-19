use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use autra_core::AgentStepper;
use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, BudgetSnapshot, CancelFlag, Disposition, EffectIntent,
    EventKind, RunId, StepError, ToolCallRequest, ToolOutcome,
};
use autra_harness::{ModelError, ModelProvider, ReActStepper, Tool, ToolRegistry};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("agent `{name}` requires a model")]
    MissingModel { name: String },
}

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub cancel: CancelFlag,
    pub effect_timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
pub enum RunEvent {
    RunStarted {
        run_id: RunId,
    },
    ModelCallStarted {
        turn: u32,
    },
    ModelResult {
        text: Option<String>,
        tool_calls: Vec<ToolCallRequest>,
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
}

impl RunOutcome {
    pub fn state(&self) -> &AgentState {
        &self.state
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

pub struct Agent {
    name: String,
    instructions: Option<String>,
    model: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
    effect_timeout: Option<Duration>,
    max_turns: u32,
    max_tool_calls: u32,
}

impl Agent {
    pub fn builder(name: impl Into<String>) -> AgentBuilder {
        AgentBuilder::new(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn run(&self, input: &str) -> Result<RunOutcome, RunError> {
        self.run_with_options(input, RunOptions::default(), |_| {})
            .await
    }

    pub async fn run_with_options<F>(
        &self,
        input: &str,
        options: RunOptions,
        mut on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let run_id = RunId::fresh();
        let mut state = AgentState::new(
            run_id.clone(),
            BudgetSnapshot::new(self.max_turns, self.max_tool_calls),
        );
        let stepper = ReActStepper::new(self.instructions.clone(), self.tools.specs());
        let effect_timeout = options.effect_timeout.or(self.effect_timeout);
        let mut queue: VecDeque<AcceptedEvent> = VecDeque::new();
        let mut seq = 0_u64;

        enqueue(
            &mut queue,
            &mut seq,
            EventKind::RunStarted {
                input: input.to_owned(),
            },
        );
        on_event(&RunEvent::RunStarted {
            run_id: run_id.clone(),
        });

        let mut cancel_queued = false;
        loop {
            if options.cancel.is_cancelled() {
                queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
            }

            let event = queue
                .pop_front()
                .ok_or_else(|| RunError::Protocol("no accepted event to process".to_owned()))?;
            let transition = match stepper.transition(&state, &event) {
                Ok(transition) => transition,
                Err(error) => {
                    let message = error.to_string();
                    state.phase = AgentPhase::Failed;
                    state.last_error = Some(message.clone());
                    on_event(&RunEvent::RunFailed {
                        message: message.clone(),
                    });
                    return Err(RunError::Step(error));
                }
            };
            state = transition.next;

            if let EventKind::ModelResult(output) = &event.kind {
                on_event(&RunEvent::ModelResult {
                    text: output.text.clone(),
                    tool_calls: output.tool_calls.clone(),
                });
            }

            match transition.disposition {
                Disposition::Finished => {
                    if state.phase == AgentPhase::Cancelled {
                        on_event(&RunEvent::RunCancelled);
                    } else {
                        on_event(&RunEvent::RunCompleted {
                            answer: state.final_answer.clone(),
                        });
                    }
                    return Ok(RunOutcome { state });
                }
                Disposition::Failed => {
                    let message = state
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "run failed".to_owned());
                    on_event(&RunEvent::RunFailed {
                        message: message.clone(),
                    });
                    return Err(RunError::Failed(message));
                }
                Disposition::Continue => {}
            }

            if options.cancel.is_cancelled() {
                queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
                continue;
            }

            for intent in transition.intents {
                if options.cancel.is_cancelled() {
                    queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
                    break;
                }
                match intent {
                    EffectIntent::CallModel(request) => {
                        on_event(&RunEvent::ModelCallStarted {
                            turn: state.budget.turns_used,
                        });
                        let output_result =
                            match with_timeout(effect_timeout, self.model.complete(&request)).await
                            {
                                Ok(result) => result,
                                Err(()) => Err(ModelError::new(
                                    "model call exceeded the configured timeout",
                                )),
                            };
                        let output = match output_result {
                            Ok(output) => output,
                            Err(error) => {
                                let message = error.to_string();
                                state.phase = AgentPhase::Failed;
                                state.last_error = Some(message.clone());
                                on_event(&RunEvent::RunFailed {
                                    message: message.clone(),
                                });
                                return Err(RunError::Model(error));
                            }
                        };
                        enqueue(&mut queue, &mut seq, EventKind::ModelResult(output));
                    }
                    EffectIntent::CallTool(spec) => {
                        let call = spec.call;
                        on_event(&RunEvent::ToolCallStarted {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                        });
                        let result: Result<String, (String, bool)> = match self
                            .tools
                            .get(&call.name)
                        {
                            Some(tool) => {
                                match with_timeout(effect_timeout, tool.call(&call.arguments)).await
                                {
                                    Ok(result) => {
                                        result.map_err(|error| (error.to_string(), false))
                                    }
                                    Err(()) => Err((
                                        format!(
                                            "tool `{}` timed out; it may still have run and its outcome is unknown",
                                            call.name
                                        ),
                                        true,
                                    )),
                                }
                            }
                            None => Err((format!("unknown tool `{}`", call.name), false)),
                        };
                        let (content, is_error, outcome_unknown) = match result {
                            Ok(content) => (content, false, false),
                            Err((message, unknown)) => (message, true, unknown),
                        };
                        let outcome = ToolOutcome {
                            call_id: call.call_id,
                            name: call.name,
                            content,
                            is_error,
                            outcome_unknown,
                        };
                        on_event(&RunEvent::ToolResult {
                            call_id: outcome.call_id.clone(),
                            name: outcome.name.clone(),
                            is_error: outcome.is_error,
                            outcome_unknown: outcome.outcome_unknown,
                            content: outcome.content.clone(),
                        });
                        enqueue(&mut queue, &mut seq, EventKind::ToolResult(outcome));
                    }
                }
            }
        }
    }
}

pub struct AgentBuilder {
    name: String,
    instructions: Option<String>,
    model: Option<Arc<dyn ModelProvider>>,
    tools: ToolRegistry,
    effect_timeout: Option<Duration>,
    max_turns: u32,
    max_tool_calls: u32,
}

impl AgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: None,
            model: None,
            tools: ToolRegistry::new(),
            effect_timeout: None,
            max_turns: 8,
            max_tool_calls: 8,
        }
    }

    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub fn model(mut self, model: impl ModelProvider + 'static) -> Self {
        self.model = Some(Arc::new(model));
        self
    }

    pub fn effect_timeout(mut self, effect_timeout: Duration) -> Self {
        self.effect_timeout = Some(effect_timeout);
        self
    }

    pub fn tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.register(tool);
        self
    }

    pub fn max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub fn max_tool_calls(mut self, max_tool_calls: u32) -> Self {
        self.max_tool_calls = max_tool_calls;
        self
    }

    pub fn build(self) -> Result<Agent, BuildError> {
        let model = self.model.ok_or_else(|| BuildError::MissingModel {
            name: self.name.clone(),
        })?;
        Ok(Agent {
            name: self.name,
            instructions: self.instructions,
            model,
            tools: self.tools,
            effect_timeout: self.effect_timeout,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
        })
    }
}

fn enqueue(queue: &mut VecDeque<AcceptedEvent>, seq: &mut u64, kind: EventKind) {
    *seq += 1;
    queue.push_back(AcceptedEvent { seq: *seq, kind });
}

fn queue_cancel(queue: &mut VecDeque<AcceptedEvent>, seq: &mut u64, cancel_queued: &mut bool) {
    if !*cancel_queued {
        enqueue(queue, seq, EventKind::CancelRequested);
        *cancel_queued = true;
    }
}

async fn with_timeout<F, T, E>(timeout: Option<Duration>, future: F) -> Result<Result<T, E>, ()>
where
    F: Future<Output = Result<T, E>>,
{
    match timeout {
        Some(duration) => tokio::time::timeout(duration, future).await.map_err(|_| ()),
        None => Ok(future.await),
    }
}

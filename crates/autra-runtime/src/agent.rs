use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, AttemptStatus, BudgetSnapshot, CancelFlag,
    Disposition, EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus, EventKind,
    HistoryItem, ModelCallSpec, RunId, SessionId, StepError, ToolCallRequest, ToolOutcome,
    TransitionProposal, TurnId,
};
use autra_harness::{
    ContextBuilder, LoadSkillTool, ModelError, ModelProvider, PlanItem, ReActStepper, SharedPlan,
    SimpleContextBuilder, Skill, SkillRegistry, Tool, ToolRegistry, UpdatePlanTool,
};
use tracing::Instrument;

use crate::approval::{ApprovalDecision, ApprovalRequest, Approver, digest_intent};
use crate::dispatcher::{
    DispatchOptions, DispatchOutcome, DispatchResult, Dispatcher, LocalDispatcher, OutboxDispatcher,
};
use crate::hooks::ObserverHook;
use crate::journal::EffectJournal;
use crate::subagent::{SubagentRegistry, SubagentTool};
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
    #[error("journal error: {0}")]
    Journal(String),
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

struct BatchSink<'a, F> {
    on_event: &'a mut F,
    queue: &'a mut VecDeque<AcceptedEvent>,
    seq: &'a mut u64,
    cancel_queued: &'a mut bool,
    run_id: &'a RunId,
    approver: Option<&'a Arc<dyn Approver>>,
    effects: &'a mut Vec<EffectRecord>,
}

impl<'a, F> BatchSink<'a, F>
where
    F: FnMut(&RunEvent),
{
    fn emit_stream(&mut self, event: &RunEvent) {
        (self.on_event)(event);
    }

    fn start_tool(&mut self, call: &ToolCallRequest) {
        self.emit_stream(&RunEvent::ToolCallStarted {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
        });
    }

    fn emit_tool_result(&mut self, outcome: &ToolOutcome) {
        emit_tool_result(&mut *self.on_event, outcome);
    }

    fn start_tool_outcome(&mut self, outcome: &ToolOutcome) {
        self.emit_stream(&RunEvent::ToolCallStarted {
            call_id: outcome.call_id.clone(),
            name: outcome.name.clone(),
        });
    }

    fn accept_outcome(&mut self, outcome: ToolOutcome) {
        enqueue(
            &mut *self.queue,
            &mut *self.seq,
            EventKind::ToolResult(outcome),
        );
    }

    fn cancel(&mut self) {
        queue_cancel(&mut *self.queue, &mut *self.seq, &mut *self.cancel_queued);
    }

    fn effect_mut(&mut self, effect_id: &EffectId) -> &mut EffectRecord {
        self.effects
            .iter_mut()
            .find(|record| &record.id == effect_id)
            .expect("effect record must exist")
    }

    fn record_tool_outcome(
        &mut self,
        effect_id: &EffectId,
        known_tool: bool,
        outcome: &ToolOutcome,
    ) {
        if outcome.outcome_unknown {
            let record = self.effect_mut(effect_id);
            record.set_status(EffectStatus::OutcomeUnknown);
            record.record_attempt(
                AttemptStatus::OutcomeUnknown,
                Some(outcome.content.clone()),
                None,
            );
        } else if !known_tool {
            let record = self.effect_mut(effect_id);
            record.set_status(EffectStatus::Failed);
            record.record_attempt(AttemptStatus::Failed, None, Some(outcome.content.clone()));
        } else {
            let error = if outcome.is_error {
                Some(outcome.content.clone())
            } else {
                None
            };
            let record = self.effect_mut(effect_id);
            record.set_status(EffectStatus::Committed);
            record.record_attempt(
                AttemptStatus::Succeeded,
                Some(outcome.content.clone()),
                error,
            );
        }
    }
}

pub struct Agent {
    name: String,
    instructions: Option<String>,
    dispatcher: Arc<dyn Dispatcher>,
    tools: ToolRegistry,
    effect_timeout: Option<Duration>,
    max_turns: u32,
    max_tool_calls: u32,
    max_parallel_tool_calls: u32,
    approver: Option<Arc<dyn Approver>>,
    skills: SkillRegistry,
    context_budget_chars: Option<usize>,
    plan: Option<SharedPlan>,
    context_builder: Option<Arc<dyn ContextBuilder>>,
    stepper: Option<Arc<dyn AgentStepper>>,
    observer: Option<Arc<dyn ObserverHook>>,
    journal: Option<Arc<dyn EffectJournal>>,
}

impl Agent {
    pub fn builder(name: impl Into<String>) -> AgentBuilder {
        AgentBuilder::new(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn plan(&self) -> Option<Vec<PlanItem>> {
        self.plan
            .as_ref()
            .map(|plan| plan.lock().expect("plan lock poisoned").clone())
    }

    pub async fn run(&self, input: &str) -> Result<RunOutcome, RunError> {
        self.run_with_options(input, RunOptions::default(), |_| {})
            .await
    }

    pub async fn run_with_options<F>(
        &self,
        input: &str,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        self.run_with_history(&[], input, options, on_event).await
    }

    #[tracing::instrument(skip_all, fields(agent = %self.name))]
    pub async fn run_with_history<F>(
        &self,
        history: &[HistoryItem],
        input: &str,
        options: RunOptions,
        mut on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let run_id = RunId::fresh();
        tracing::info!(run_id = %run_id, "agent run started");
        if let Some(observer) = &self.observer {
            observer.on_run_started(&run_id);
        }
        let mut state = AgentState::new(
            run_id.clone(),
            BudgetSnapshot::new(self.max_turns, self.max_tool_calls),
        );
        state.history = history.to_vec();
        state.session_id = options.session_id.clone();
        state.turn_id = options.turn_id.clone();
        let mut effects: Vec<EffectRecord> = Vec::new();
        let stepper: Arc<dyn AgentStepper> = match &self.stepper {
            Some(stepper) => stepper.clone(),
            None => {
                let context_builder: Arc<dyn ContextBuilder> = match &self.context_builder {
                    Some(builder) => builder.clone(),
                    None => {
                        let mut builder = SimpleContextBuilder::new(self.skills.index());
                        if let Some(max_chars) = self.context_budget_chars {
                            builder = builder.with_max_chars(max_chars);
                        }
                        Arc::new(builder)
                    }
                };
                Arc::new(
                    ReActStepper::new(self.instructions.clone(), self.tools.specs())
                        .with_shared_context_builder(context_builder),
                )
            }
        };
        let effect_timeout = options.effect_timeout.or(self.effect_timeout);
        let parallel_limit = options
            .max_parallel_tool_calls
            .unwrap_or(self.max_parallel_tool_calls)
            .max(1);
        let approver = options.approver.clone().or_else(|| self.approver.clone());
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
            let (intents, disposition) = accept_local(&mut state, transition);
            if let Some(journal) = &self.journal {
                journal
                    .commit_event(&run_id, &event, &state, &effects)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }

            if let EventKind::ModelResult(output) = &event.kind {
                on_event(&RunEvent::ModelResult {
                    text: output.text.clone(),
                    tool_calls: output.tool_calls.clone(),
                });
            }

            match disposition {
                Disposition::Finished => {
                    if state.phase == AgentPhase::Cancelled {
                        on_event(&RunEvent::RunCancelled);
                    } else {
                        on_event(&RunEvent::RunCompleted {
                            answer: state.final_answer.clone(),
                        });
                    }
                    tracing::info!(phase = ?state.phase, "agent run finished");
                    if let Some(observer) = &self.observer {
                        observer.on_run_finished(&run_id, &state.phase);
                    }
                    if let Some(journal) = &self.journal {
                        journal
                            .save_run(&state)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                        for effect in &effects {
                            journal
                                .record(&run_id, effect)
                                .map_err(|error| RunError::Journal(error.to_string()))?;
                        }
                    }
                    return Ok(RunOutcome { state, effects });
                }
                Disposition::Failed => {
                    let message = state
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "run failed".to_owned());
                    on_event(&RunEvent::RunFailed {
                        message: message.clone(),
                    });
                    tracing::warn!(message = %message, "agent run failed");
                    if let Some(observer) = &self.observer {
                        observer.on_run_finished(&run_id, &state.phase);
                    }
                    return Err(RunError::Failed(message));
                }
                Disposition::Continue => {}
            }

            if options.cancel.is_cancelled() {
                queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
                continue;
            }

            if let Some(observer) = &self.observer {
                for intent in &intents {
                    observer.on_effect_accepted(&run_id, intent);
                }
            }
            let mut tool_calls: Vec<(EffectId, ToolCallRequest)> = Vec::new();
            let mut model_requests: Vec<(EffectId, ModelCallSpec)> = Vec::new();
            for intent in intents {
                let effect_id = EffectId::fresh();
                let digest = digest_intent(&intent);
                let intent_json = serde_json::to_string(&intent).unwrap_or_default();
                let record = match &intent {
                    EffectIntent::CallModel(_) => EffectRecord::new(
                        effect_id.clone(),
                        EffectKind::ModelCall,
                        None,
                        None,
                        digest,
                    ),
                    EffectIntent::CallTool(spec) => EffectRecord::new(
                        effect_id.clone(),
                        EffectKind::ToolCall,
                        Some(spec.call.name.clone()),
                        Some(spec.call.call_id.clone()),
                        digest,
                    ),
                }
                .with_intent_json(intent_json);
                effects.push(record);
                match intent {
                    EffectIntent::CallTool(spec) => tool_calls.push((effect_id, spec.call)),
                    EffectIntent::CallModel(request) => model_requests.push((effect_id, request)),
                }
            }

            if !tool_calls.is_empty() {
                let mut sink = BatchSink {
                    on_event: &mut on_event,
                    queue: &mut queue,
                    seq: &mut seq,
                    cancel_queued: &mut cancel_queued,
                    run_id: &run_id,
                    approver: approver.as_ref(),
                    effects: &mut effects,
                };
                self.execute_tool_batch(
                    tool_calls,
                    parallel_limit,
                    effect_timeout,
                    &options.cancel,
                    &mut sink,
                )
                .await?;
            }

            for (effect_id, request) in model_requests {
                if options.cancel.is_cancelled() {
                    effect_mut(&mut effects, &effect_id)
                        .set_status(EffectStatus::CancelledBeforeDispatch);
                    queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
                    break;
                }
                effect_mut(&mut effects, &effect_id).set_status(EffectStatus::Authorized);
                on_event(&RunEvent::ModelCallStarted {
                    turn: state.budget.turns_used,
                });
                if let Some(observer) = &self.observer {
                    observer.on_model_request(&run_id, state.budget.turns_used);
                }
                let model_span =
                    tracing::info_span!("gen_ai.model.call", turn = state.budget.turns_used);
                let output_result = async {
                    match &self.journal {
                        Some(journal) => {
                            let record = effect_mut(&mut effects, &effect_id).clone();
                            journal
                                .record(&run_id, &record)
                                .map_err(|error| RunError::Journal(error.to_string()))?;
                            journal
                                .enqueue_effect(&run_id, &record)
                                .map_err(|error| RunError::Journal(error.to_string()))?;
                            let results = OutboxDispatcher::run_once_for_run(
                                journal.as_ref(),
                                &run_id,
                                1,
                                self.dispatcher.as_ref(),
                                effect_timeout,
                            )
                            .await
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                            sync_effects(journal.as_ref(), &run_id, &mut effects)?;
                            let result = results
                                .into_iter()
                                .find(|result| result.effect_id == effect_id)
                                .ok_or_else(|| {
                                    RunError::Protocol(
                                        "the claimed model effect produced no outcome".to_owned(),
                                    )
                                })?;
                            match result.outcome {
                                DispatchOutcome::Model(output) => output.map_err(RunError::Model),
                                _ => Err(RunError::Protocol(
                                    "the model effect produced a non-model outcome".to_owned(),
                                )),
                            }
                        }
                        None => {
                            effect_mut(&mut effects, &effect_id)
                                .set_status(EffectStatus::Dispatched);
                            self.dispatcher
                                .call_model(&request, effect_timeout)
                                .await
                                .map_err(RunError::Model)
                        }
                    }
                }
                .instrument(model_span)
                .await;
                let output = match output_result {
                    Ok(output) => {
                        if self.journal.is_none() {
                            let record = effect_mut(&mut effects, &effect_id);
                            record.set_status(EffectStatus::Committed);
                            record.record_attempt(
                                AttemptStatus::Succeeded,
                                output.text.clone(),
                                None,
                            );
                        }
                        output
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if self.journal.is_none() {
                            let record = effect_mut(&mut effects, &effect_id);
                            record.set_status(EffectStatus::Failed);
                            record.record_attempt(
                                AttemptStatus::Failed,
                                None,
                                Some(message.clone()),
                            );
                        }
                        state.phase = AgentPhase::Failed;
                        state.last_error = Some(message.clone());
                        on_event(&RunEvent::RunFailed {
                            message: message.clone(),
                        });
                        if let Some(observer) = &self.observer {
                            observer.on_run_finished(&run_id, &state.phase);
                        }
                        return Err(error);
                    }
                };
                enqueue(&mut queue, &mut seq, EventKind::ModelResult(output));
            }
        }
    }
    async fn execute_tool_batch<F>(
        &self,
        calls: Vec<(EffectId, ToolCallRequest)>,
        limit: u32,
        effect_timeout: Option<Duration>,
        cancel: &CancelFlag,
        sink: &mut BatchSink<'_, F>,
    ) -> Result<(), RunError>
    where
        F: FnMut(&RunEvent),
    {
        let limit = limit.max(1) as usize;
        let mut outcomes: Vec<Option<ToolOutcome>> = vec![None; calls.len()];
        let mut approved: Vec<Option<(EffectId, ToolCallRequest)>> = vec![None; calls.len()];
        let mut cancelled = false;

        for (index, (effect_id, call)) in calls.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let requires_approval = self
                .tools
                .get(&call.name)
                .map(|tool| tool.contract().requires_approval)
                .unwrap_or(false);
            if requires_approval {
                sink.effect_mut(effect_id)
                    .set_status(EffectStatus::AwaitingApproval);
                let preview = self
                    .tools
                    .get(&call.name)
                    .and_then(|tool| tool.preview(&call.arguments));
                let request =
                    ApprovalRequest::new(sink.run_id, effect_id, call).with_preview(preview);
                sink.emit_stream(&RunEvent::ApprovalRequested {
                    call_id: request.call_id.clone(),
                    tool_name: request.tool_name.clone(),
                    argument_digest: request.argument_digest.clone(),
                });
                let decision = match sink.approver {
                    Some(approver) => approver.approve(&request).await,
                    None => ApprovalDecision::Rejected,
                };
                let is_approved = decision == ApprovalDecision::Approved;
                sink.emit_stream(&RunEvent::ApprovalDecided {
                    call_id: request.call_id.clone(),
                    approved: is_approved,
                });
                if !is_approved {
                    let outcome = rejected_outcome(call);
                    let record = sink.effect_mut(effect_id);
                    record.set_status(EffectStatus::Rejected);
                    record.error = Some(outcome.content.clone());
                    sink.emit_tool_result(&outcome);
                    outcomes[index] = Some(outcome);
                    continue;
                }
            }
            sink.effect_mut(effect_id)
                .set_status(EffectStatus::Authorized);
            approved[index] = Some((effect_id.clone(), call.clone()));
        }

        let approved_calls: Vec<(usize, EffectId, ToolCallRequest)> = approved
            .into_iter()
            .enumerate()
            .filter_map(|(index, call)| call.map(|(effect_id, call)| (index, effect_id, call)))
            .collect();

        if cancelled {
            mark_cancelled_before_dispatch(sink);
            for outcome in outcomes.into_iter().flatten() {
                sink.accept_outcome(outcome);
            }
            sink.cancel();
            return Ok(());
        }

        if let Some(journal) = &self.journal {
            let (was_cancelled, dispatched) = self
                .execute_claimed_tool_batch(
                    journal.as_ref(),
                    approved_calls,
                    limit,
                    effect_timeout,
                    cancel,
                    sink,
                )
                .await?;
            cancelled = was_cancelled;
            for (index, outcome) in dispatched {
                outcomes[index] = Some(outcome);
            }
        } else if limit == 1 || approved_calls.len() == 1 {
            for (index, effect_id, call) in approved_calls {
                if cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let known_tool = self.tools.get(&call.name).is_some();
                sink.effect_mut(&effect_id)
                    .set_status(EffectStatus::Dispatched);
                sink.start_tool(&call);
                let outcome = self
                    .dispatcher
                    .call_tool(sink.run_id, &call, effect_timeout)
                    .await;
                sink.record_tool_outcome(&effect_id, known_tool, &outcome);
                sink.emit_tool_result(&outcome);
                outcomes[index] = Some(outcome);
            }
        } else {
            for chunk in approved_calls.chunks(limit) {
                let mut tasks = tokio::task::JoinSet::new();
                for (index, effect_id, call) in chunk {
                    if cancel.is_cancelled() {
                        cancelled = true;
                        break;
                    }
                    let known_tool = self.tools.get(&call.name).is_some();
                    sink.effect_mut(effect_id)
                        .set_status(EffectStatus::Dispatched);
                    sink.start_tool(call);
                    let index = *index;
                    let effect_id = effect_id.clone();
                    let dispatcher = self.dispatcher.clone();
                    let call = call.clone();
                    let run_id = sink.run_id.clone();
                    tasks.spawn(async move {
                        (
                            index,
                            effect_id,
                            known_tool,
                            dispatcher.call_tool(&run_id, &call, effect_timeout).await,
                        )
                    });
                }
                while let Some(result) = tasks.join_next().await {
                    let (index, effect_id, known_tool, outcome) = result.map_err(|error| {
                        RunError::Protocol(format!("tool task failed: {error}"))
                    })?;
                    sink.record_tool_outcome(&effect_id, known_tool, &outcome);
                    sink.emit_tool_result(&outcome);
                    outcomes[index] = Some(outcome);
                }
                if cancelled || cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
            }
        }

        if cancelled || cancel.is_cancelled() {
            mark_cancelled_before_dispatch(sink);
        }
        for outcome in outcomes.into_iter().flatten() {
            sink.accept_outcome(outcome);
        }
        if cancelled || cancel.is_cancelled() {
            sink.cancel();
        }
        Ok(())
    }

    async fn execute_claimed_tool_batch<F>(
        &self,
        journal: &dyn EffectJournal,
        approved: Vec<(usize, EffectId, ToolCallRequest)>,
        limit: usize,
        effect_timeout: Option<Duration>,
        cancel: &CancelFlag,
        sink: &mut BatchSink<'_, F>,
    ) -> Result<(bool, Vec<(usize, ToolOutcome)>), RunError>
    where
        F: FnMut(&RunEvent),
    {
        let run_id = sink.run_id.clone();
        let mut index_by_effect = std::collections::HashMap::new();
        for (index, effect_id, _) in &approved {
            let record = sink.effect_mut(effect_id).clone();
            journal
                .record(&run_id, &record)
                .map_err(|error| RunError::Journal(error.to_string()))?;
            journal
                .enqueue_effect(&run_id, &record)
                .map_err(|error| RunError::Journal(error.to_string()))?;
            index_by_effect.insert(effect_id.clone(), *index);
        }
        let mut cancelled = false;
        let mut dispatched = Vec::new();
        {
            let mut on_result = |result: &DispatchResult| match &result.outcome {
                DispatchOutcome::Tool(outcome) => {
                    sink.start_tool_outcome(outcome);
                    sink.emit_tool_result(outcome);
                    if let Some(index) = index_by_effect.get(&result.effect_id) {
                        dispatched.push((*index, outcome.clone()));
                    }
                }
                DispatchOutcome::Cancelled => cancelled = true,
                DispatchOutcome::Model(_) => {}
            };
            let options = DispatchOptions {
                limit: approved.len(),
                max_parallel: limit.max(1),
                timeout: effect_timeout,
                cancel: Some(cancel),
            };
            OutboxDispatcher::run_once_for_run_with(
                journal,
                &run_id,
                options,
                self.dispatcher.as_ref(),
                &mut on_result,
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()))?;
        }
        sync_effects(journal, &run_id, sink.effects)?;
        Ok((cancelled || cancel.is_cancelled(), dispatched))
    }
}

pub struct AgentBuilder {
    name: String,
    instructions: Option<String>,
    model: Option<Arc<dyn ModelProvider>>,
    dispatcher: Option<Arc<dyn Dispatcher>>,
    tools: ToolRegistry,
    effect_timeout: Option<Duration>,
    max_turns: u32,
    max_tool_calls: u32,
    max_parallel_tool_calls: u32,
    approver: Option<Arc<dyn Approver>>,
    skills: SkillRegistry,
    context_budget_chars: Option<usize>,
    planning: bool,
    subagents: SubagentRegistry,
    context_builder: Option<Arc<dyn ContextBuilder>>,
    stepper: Option<Arc<dyn AgentStepper>>,
    observer: Option<Arc<dyn ObserverHook>>,
    journal: Option<Arc<dyn EffectJournal>>,
}

impl AgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: None,
            model: None,
            dispatcher: None,
            tools: ToolRegistry::new(),
            effect_timeout: None,
            max_turns: 8,
            max_tool_calls: 8,
            max_parallel_tool_calls: 1,
            approver: None,
            skills: SkillRegistry::new(),
            context_budget_chars: None,
            planning: false,
            subagents: SubagentRegistry::new(),
            context_builder: None,
            stepper: None,
            observer: None,
            journal: None,
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

    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
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

    pub fn approver(mut self, approver: impl Approver + 'static) -> Self {
        self.approver = Some(Arc::new(approver));
        self
    }

    pub fn max_parallel_tool_calls(mut self, max_parallel_tool_calls: u32) -> Self {
        self.max_parallel_tool_calls = max_parallel_tool_calls.max(1);
        self
    }

    pub fn max_tool_calls(mut self, max_tool_calls: u32) -> Self {
        self.max_tool_calls = max_tool_calls;
        self
    }

    pub fn skill(mut self, skill: Skill) -> Self {
        self.skills.register(skill);
        self
    }

    pub fn subagent(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        agent: Agent,
    ) -> Self {
        self.subagents.register(name, description, agent);
        self
    }

    pub fn planning(mut self) -> Self {
        self.planning = true;
        self
    }

    /// Use a custom ContextBuilder. The builder owns skills and budget composition.
    pub fn context_builder(mut self, builder: impl ContextBuilder + 'static) -> Self {
        self.context_builder = Some(Arc::new(builder));
        self
    }

    /// Replace the ReAct Stepper. The custom engine owns its decisions; the
    /// coordinator still dispatches emitted intents and enforces budgets.
    pub fn stepper(mut self, stepper: impl AgentStepper + 'static) -> Self {
        self.stepper = Some(Arc::new(stepper));
        self
    }

    pub fn observer(mut self, observer: impl ObserverHook + 'static) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    pub fn dispatcher(mut self, dispatcher: impl Dispatcher + 'static) -> Self {
        self.dispatcher = Some(Arc::new(dispatcher));
        self
    }

    pub fn journal(mut self, journal: impl EffectJournal + 'static) -> Self {
        self.journal = Some(Arc::new(journal));
        self
    }

    pub fn context_budget_chars(mut self, max_chars: usize) -> Self {
        self.context_budget_chars = Some(max_chars);
        self
    }

    pub fn build(self) -> Result<Agent, BuildError> {
        let mut tools = self.tools;
        if !self.skills.is_empty() {
            tools.register(LoadSkillTool::new(Arc::new(self.skills.clone())));
        }
        if !self.subagents.is_empty() {
            tools.register(SubagentTool::new(self.subagents.clone()));
        }
        let (instructions, plan) = if self.planning {
            let plan: SharedPlan = Arc::new(Mutex::new(Vec::new()));
            tools.register(UpdatePlanTool::new(plan.clone()));
            let instructions = match self.instructions {
                Some(instructions) => format!(
                    "{instructions}

Maintain a short plan with `update_plan` and keep it current."
                ),
                None => "Maintain a short plan with `update_plan` and keep it current.".to_owned(),
            };
            (Some(instructions), Some(plan))
        } else {
            (self.instructions, None)
        };
        let dispatcher = match self.dispatcher {
            Some(dispatcher) => dispatcher,
            None => {
                let model = self.model.ok_or_else(|| BuildError::MissingModel {
                    name: self.name.clone(),
                })?;
                Arc::new(LocalDispatcher::new(model, tools.clone()))
            }
        };
        Ok(Agent {
            name: self.name,
            instructions,
            dispatcher,
            tools,
            effect_timeout: self.effect_timeout,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
            max_parallel_tool_calls: self.max_parallel_tool_calls,
            approver: self.approver,
            skills: self.skills,
            context_budget_chars: self.context_budget_chars,
            plan,
            context_builder: self.context_builder,
            stepper: self.stepper,
            observer: self.observer,
            journal: self.journal,
        })
    }
}

fn sync_effects(
    journal: &dyn EffectJournal,
    run_id: &RunId,
    effects: &mut [EffectRecord],
) -> Result<(), RunError> {
    let stored = journal
        .load(run_id)
        .map_err(|error| RunError::Journal(error.to_string()))?;
    for record in effects.iter_mut() {
        if let Some(updated) = stored.iter().find(|stored| stored.id == record.id) {
            *record = updated.clone();
        }
    }
    Ok(())
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

fn emit_tool_result<F: FnMut(&RunEvent)>(on_event: &mut F, outcome: &ToolOutcome) {
    on_event(&RunEvent::ToolResult {
        call_id: outcome.call_id.clone(),
        name: outcome.name.clone(),
        is_error: outcome.is_error,
        outcome_unknown: outcome.outcome_unknown,
        content: outcome.content.clone(),
    });
}

fn rejected_outcome(call: &ToolCallRequest) -> ToolOutcome {
    ToolOutcome {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        content: "approval rejected; the tool was not executed".to_owned(),
        is_error: true,
        outcome_unknown: false,
    }
}

fn effect_mut<'a>(effects: &'a mut [EffectRecord], effect_id: &EffectId) -> &'a mut EffectRecord {
    effects
        .iter_mut()
        .find(|record| &record.id == effect_id)
        .expect("effect record must exist")
}

fn mark_cancelled_before_dispatch<F: FnMut(&RunEvent)>(sink: &mut BatchSink<'_, F>) {
    for record in sink.effects.iter_mut() {
        if matches!(
            record.status,
            EffectStatus::Recorded | EffectStatus::AwaitingApproval | EffectStatus::Authorized
        ) {
            record.set_status(EffectStatus::CancelledBeforeDispatch);
        }
    }
}

fn accept_local(
    state: &mut AgentState,
    proposal: TransitionProposal,
) -> (Vec<EffectIntent>, Disposition) {
    // LocalEphemeral is the single writer; a durable host must commit the
    // state and effect intents atomically at this boundary instead.
    *state = proposal.next;
    (proposal.intents, proposal.disposition)
}

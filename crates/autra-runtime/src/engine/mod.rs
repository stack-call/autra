//! Local execution engine: coordinator, effect runner and recovery.
//!
//! It depends on core and execution contracts only; harness assembly belongs
//! to the product facade.

mod approval;
mod policy;
mod recovery;

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, BudgetHandle, BudgetSnapshot, CancelFlag,
    Disposition, EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus, EventKind,
    HistoryItem, ModelCallSpec, RunId, ToolCallRequest, ToolOutcome, TransitionProposal,
};
use autra_execution::{
    Approver, ChildExecutionContext, DurableTransitionStore, EffectJournal, ObserverHook,
    PolicyChain, PolicyDecision, RunError, RunEvent, RunOptions, RunOutcome, RunSnapshot,
    ToolContext, ToolPolicy, ToolRegistry,
};
use tracing::Instrument;

use crate::approval::{ApprovalDecision, digest_intent};
use crate::child::{ChildHandle, ChildSpec};
use crate::dispatcher::{
    DEFAULT_CLAIM_LEASE, DeltaSink, DispatchOptions, DispatchOutcome, DispatchResult, Dispatcher,
    OutboxDispatcher,
};
use crate::hub::RunHub;

/// Drives one Run: accepts events, commits transitions, executes effects and
/// resumes committed checkpoints.
#[derive(Clone)]
pub struct LocalEngine {
    dispatcher: Arc<dyn Dispatcher>,
    tools: ToolRegistry,
    journal: Option<Arc<dyn DurableTransitionStore>>,
    observer: Option<Arc<dyn ObserverHook>>,
    max_turns: u32,
    max_tool_calls: u32,
    max_parallel_tool_calls: u32,
    effect_timeout: Option<Duration>,
    approver: Option<Arc<dyn Approver>>,
    policy: PolicyChain,
    actor: Option<String>,
    approval_ttl: Option<Duration>,
    hub: Option<Arc<RunHub>>,
}

impl LocalEngine {
    pub fn new(dispatcher: Arc<dyn Dispatcher>, tools: ToolRegistry) -> Self {
        Self {
            dispatcher,
            tools,
            journal: None,
            observer: None,
            max_turns: 8,
            max_tool_calls: 8,
            max_parallel_tool_calls: 1,
            effect_timeout: None,
            approver: None,
            policy: PolicyChain::new(),
            actor: None,
            approval_ttl: None,
            hub: None,
        }
    }

    pub fn with_journal(mut self, journal: Arc<dyn DurableTransitionStore>) -> Self {
        self.journal = Some(journal);
        self
    }

    pub fn with_observer(mut self, observer: Arc<dyn ObserverHook>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn with_limits(
        mut self,
        max_turns: u32,
        max_tool_calls: u32,
        max_parallel_tool_calls: u32,
    ) -> Self {
        self.max_turns = max_turns;
        self.max_tool_calls = max_tool_calls;
        self.max_parallel_tool_calls = max_parallel_tool_calls.max(1);
        self
    }

    pub fn with_effect_timeout(mut self, effect_timeout: Option<Duration>) -> Self {
        self.effect_timeout = effect_timeout;
        self
    }

    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Add a policy. Policies only narrow the host baseline.
    pub fn with_policy(mut self, policy: impl ToolPolicy + 'static) -> Self {
        self.policy = self.policy.push(policy);
        self
    }

    pub fn with_policy_chain(mut self, policy: PolicyChain) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn with_approval_ttl(mut self, ttl: Duration) -> Self {
        self.approval_ttl = Some(ttl);
        self
    }

    pub fn with_run_hub(mut self, hub: Arc<RunHub>) -> Self {
        self.hub = Some(hub);
        self
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Run one new Run from its initial event.
    pub async fn run<F>(
        &self,
        stepper: Arc<dyn AgentStepper>,
        history: &[HistoryItem],
        input: &str,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        self.run_with_identity(
            RunId::fresh(),
            None,
            self.max_budget(),
            stepper,
            history.to_vec(),
            input.to_owned(),
            options,
            on_event,
        )
        .await
    }

    /// The budget a new root Run receives from this engine.
    pub fn max_budget(&self) -> BudgetSnapshot {
        BudgetSnapshot::new(self.max_turns, self.max_tool_calls)
    }

    /// Spawn a child Run on this engine. The child gets a fresh Run id, a
    /// parent link and a cancellation flag that inherits the parent's.
    pub fn spawn_child(&self, spec: ChildSpec) -> ChildHandle {
        let run_id = RunId::fresh();
        let ChildSpec {
            stepper,
            input,
            history,
            budget,
            parent_run_id,
            cancel,
            execution,
            forward_events,
        } = spec;
        let child_cancel = CancelFlag::child_of(&cancel);
        let handle_cancel = child_cancel.clone();
        let handle_run_id = run_id.clone();
        let mut engine = self.clone();
        if let Some(journal) = execution.journal {
            engine = engine.with_journal(journal);
        }
        if let Some(approver) = execution.approver {
            engine = engine.with_approver(approver);
        }
        let (sender, receiver) = if forward_events {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let join = tokio::spawn(async move {
            let options = RunOptions {
                cancel: child_cancel,
                ..RunOptions::default()
            };
            let on_event = move |event: &RunEvent| {
                if let Some(sender) = &sender {
                    let _ = sender.send(event.clone());
                }
            };
            engine
                .run_with_identity(
                    run_id,
                    Some(parent_run_id),
                    budget,
                    stepper,
                    history,
                    input,
                    options,
                    on_event,
                )
                .await
        });
        ChildHandle::new(handle_run_id, handle_cancel, join, receiver)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_with_identity<F>(
        &self,
        run_id: RunId,
        parent_run_id: Option<RunId>,
        budget: BudgetSnapshot,
        stepper: Arc<dyn AgentStepper>,
        history: Vec<HistoryItem>,
        input: String,
        options: RunOptions,
        mut on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        tracing::info!(run_id = %run_id, "agent run started");
        if let Some(observer) = &self.observer {
            observer.on_run_started(&run_id);
        }
        let mut state = AgentState::new(run_id.clone(), budget);
        state.parent_run_id = parent_run_id;
        stepper.initialize(&mut state, &history);
        if let Some(hub) = &self.hub {
            hub.start(RunSnapshot::from_state(&state, 0, 0));
        }
        state.session_id = options.session_id.clone();
        state.turn_id = options.turn_id.clone();
        let mut queue: VecDeque<AcceptedEvent> = VecDeque::new();
        let mut seq = 0_u64;
        enqueue(&mut queue, &mut seq, EventKind::RunStarted { input });
        on_event(&RunEvent::RunStarted {
            run_id: run_id.clone(),
        });
        self.drive(stepper, state, queue, Vec::new(), options, on_event)
            .await
    }

    /// Continue a Run from its last committed checkpoint. Requires a journal.
    pub async fn drain_outbox(&self, limit: usize) -> Result<Vec<DispatchResult>, RunError> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("outbox drain requires a journal".to_owned()))?;
        OutboxDispatcher::run_due(
            journal.journal(),
            limit,
            self.dispatcher.as_ref(),
            self.effect_timeout,
        )
        .await
        .map_err(|error| RunError::Journal(error.to_string()))
    }

    async fn drive<F>(
        &self,
        stepper: Arc<dyn AgentStepper>,
        mut state: AgentState,
        mut queue: VecDeque<AcceptedEvent>,
        mut effects: Vec<EffectRecord>,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let effect_timeout = options.effect_timeout.or(self.effect_timeout);
        let parallel_limit = options
            .max_parallel_tool_calls
            .unwrap_or(self.max_parallel_tool_calls)
            .max(1);
        let approver = options.approver.clone().or_else(|| self.approver.clone());
        let run_id = state.run_id.clone();
        let hub = self.hub.clone();
        let mut forward = on_event;
        let mut on_event = |event: &RunEvent| {
            if let Some(hub) = &hub {
                hub.publish(&run_id, event);
            }
            forward(event);
        };
        let mut seq = queue.iter().map(|event| event.seq).max().unwrap_or(0);
        let mut hub_guard = HubGuard::new(
            self.hub.clone(),
            RunSnapshot::from_state(&state, seq, effects.len()),
        );
        let mut cancel_queued = false;
        loop {
            if options.cancel.is_cancelled() {
                queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
            }

            let event = queue
                .pop_front()
                .ok_or_else(|| RunError::Protocol("no accepted event to process".to_owned()))?;
            let expected_version = state.state_version;
            let transition = match stepper.transition(&state, &event) {
                Ok(transition) => transition,
                Err(error) => {
                    let message = error.to_string();
                    state.phase = AgentPhase::Failed;
                    state.last_error = Some(message.clone());
                    on_event(&RunEvent::RunFailed {
                        message: message.clone(),
                    });
                    hub_guard.update(RunSnapshot::from_state(&state, event.seq, effects.len()));
                    hub_guard.finish();
                    return Err(RunError::Step(error));
                }
            };
            let (intents, disposition) = accept_local(&mut state, transition, expected_version);

            let first_new = effects.len();
            let mut tool_calls: Vec<(EffectId, ToolCallRequest)> = Vec::new();
            let mut model_requests: Vec<(EffectId, ModelCallSpec)> = Vec::new();
            for intent in intents {
                if let Some(observer) = &self.observer {
                    observer.on_effect_accepted(&run_id, &intent);
                }
                let effect_id = EffectId::fresh();
                let digest = digest_intent(&intent);
                let intent_json = serde_json::to_string(&intent).unwrap_or_default();
                let requires_approval = match &intent {
                    EffectIntent::CallModel(_) => false,
                    EffectIntent::CallTool(spec) => self.tool_needs_gate(&run_id, &spec.call),
                };
                let mut record = match &intent {
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
                record.set_status(if requires_approval {
                    EffectStatus::AwaitingApproval
                } else {
                    EffectStatus::Authorized
                });
                effects.push(record);
                match intent {
                    EffectIntent::CallTool(spec) => tool_calls.push((effect_id, spec.call)),
                    EffectIntent::CallModel(request) => model_requests.push((effect_id, request)),
                }
            }
            if let Some(journal) = &self.journal {
                journal
                    .commit_transition(
                        &run_id,
                        expected_version,
                        &event,
                        &state,
                        &effects[first_new..],
                    )
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
            hub_guard.update(RunSnapshot::from_state(&state, event.seq, effects.len()));

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
                    hub_guard.finish();
                    return Ok(RunOutcome::new(state, effects));
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
                    hub_guard.finish();
                    return Err(RunError::Failed(message));
                }
                Disposition::Continue => {}
            }

            if options.cancel.is_cancelled() {
                cancel_staged_effects(&mut effects[first_new..], self.journal.as_deref(), &run_id)?;
                queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
                continue;
            }

            if !tool_calls.is_empty() {
                let budget = BudgetHandle::new(state.budget.clone());
                let mut sink = BatchSink {
                    on_event: &mut on_event,
                    queue: &mut queue,
                    seq: &mut seq,
                    cancel_queued: &mut cancel_queued,
                    run_id: &run_id,
                    approver: approver.as_ref(),
                    effects: &mut effects,
                    budget: &budget,
                };
                self.execute_tool_batch(
                    tool_calls,
                    parallel_limit,
                    effect_timeout,
                    &options.cancel,
                    &mut sink,
                )
                .await?;
                state.budget = budget.snapshot();
            }

            for (effect_id, request) in model_requests {
                if options.cancel.is_cancelled() {
                    let record = effect_mut(&mut effects, &effect_id);
                    record.set_status(EffectStatus::CancelledBeforeDispatch);
                    if let Some(journal) = &self.journal {
                        journal
                            .record(&run_id, record)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                        journal
                            .complete_outbox(&effect_id)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                    }
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
                    let (delta, mut deltas) = delta_channel();
                    match &self.journal {
                        Some(journal) => {
                            let options = DispatchOptions {
                                limit: 1,
                                max_parallel: 1,
                                timeout: effect_timeout,
                                cancel: None,
                                lease: DEFAULT_CLAIM_LEASE,
                                budget: None,
                                child_execution: None,
                                delta: Some(delta),
                            };
                            let dispatch = OutboxDispatcher::run_once_for_run_with(
                                journal.as_ref(),
                                &run_id,
                                options,
                                self.dispatcher.as_ref(),
                                |_| {},
                            );
                            let results = await_with_deltas(dispatch, &mut deltas, &mut on_event)
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
                            let mut emit = move |text: String| delta(text);
                            let call = self.dispatcher.call_model_streaming(
                                &request,
                                effect_timeout,
                                &mut emit,
                            );
                            await_with_deltas(call, &mut deltas, &mut on_event)
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
                            effect_mut(&mut effects, &effect_id)
                                .apply_model_success(output.text.clone());
                        }
                        output
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if self.journal.is_none() {
                            effect_mut(&mut effects, &effect_id)
                                .apply_model_failure(message.clone());
                        }
                        state.phase = AgentPhase::Failed;
                        state.last_error = Some(message.clone());
                        on_event(&RunEvent::RunFailed {
                            message: message.clone(),
                        });
                        if let Some(observer) = &self.observer {
                            observer.on_run_finished(&run_id, &state.phase);
                        }
                        hub_guard.update(RunSnapshot::from_state(&state, event.seq, effects.len()));
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
        let run_id = sink.run_id.clone();
        let child_execution = ChildExecutionContext {
            journal: self.journal.clone(),
            approver: self.approver.clone(),
        };
        let mut outcomes: Vec<Option<ToolOutcome>> = vec![None; calls.len()];
        let mut approved: Vec<Option<(EffectId, ToolCallRequest)>> = vec![None; calls.len()];
        let mut cancelled = false;

        for (index, (effect_id, call)) in calls.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let decision = match self.policy_decision(&run_id, call) {
                Some(decision) => decision,
                None => {
                    // Unknown tools are dispatched so the outcome is a normal
                    // tool error, not an approval prompt.
                    sink.effect_mut(effect_id)
                        .set_status(EffectStatus::Authorized);
                    approved[index] = Some((effect_id.clone(), call.clone()));
                    continue;
                }
            };
            if decision == PolicyDecision::Deny {
                let outcome = policy_denied_outcome(call, &self.policy.revision());
                let record = sink.effect_mut(effect_id);
                record.set_status(EffectStatus::Rejected);
                record.error = Some(outcome.content.clone());
                if let Some(journal) = &self.journal {
                    journal
                        .record(&run_id, record)
                        .map_err(|error| RunError::Journal(error.to_string()))?;
                }
                sink.emit_tool_result(&outcome);
                outcomes[index] = Some(outcome);
                continue;
            }
            if decision == PolicyDecision::Ask {
                sink.effect_mut(effect_id)
                    .set_status(EffectStatus::AwaitingApproval);
                let request = self.approval_request(sink.run_id, effect_id, call);
                sink.emit_stream(&RunEvent::ApprovalRequested {
                    call_id: request.call_id.clone(),
                    tool_name: request.tool_name.clone(),
                    argument_digest: request.argument_digest.clone(),
                });
                let decision = self
                    .resolve_approval(&request, effect_id, sink.approver)
                    .await?;
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
                    if let Some(journal) = &self.journal {
                        journal
                            .record(&run_id, record)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                    }
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
            mark_cancelled_before_dispatch(sink, self.journal.as_deref())?;
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
                let context = ToolContext {
                    run_id: sink.run_id.clone(),
                    call_id: call.call_id.clone(),
                    parent_run_id: Some(sink.run_id.clone()),
                    cancel: Some(cancel.clone()),
                    budget: Some(sink.budget.clone()),
                    child_execution: Some(child_execution.clone()),
                };
                let outcome = self
                    .dispatcher
                    .call_tool_with_context(&context, &call, effect_timeout)
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
                    let context = ToolContext {
                        run_id: run_id.clone(),
                        call_id: call.call_id.clone(),
                        parent_run_id: Some(run_id.clone()),
                        cancel: Some(cancel.clone()),
                        budget: Some(sink.budget.clone()),
                        child_execution: Some(child_execution.clone()),
                    };
                    tasks.spawn(async move {
                        (
                            index,
                            effect_id,
                            known_tool,
                            dispatcher
                                .call_tool_with_context(&context, &call, effect_timeout)
                                .await,
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
            mark_cancelled_before_dispatch(sink, self.journal.as_deref())?;
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
        journal: &dyn DurableTransitionStore,
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
        let budget = sink.budget.clone();
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
                DispatchOutcome::Model(_)
                | DispatchOutcome::AlreadyResolved
                | DispatchOutcome::Interrupted => {}
            };
            let options = DispatchOptions {
                limit: approved.len(),
                max_parallel: limit.max(1),
                timeout: effect_timeout,
                cancel: Some(cancel),
                lease: DEFAULT_CLAIM_LEASE,
                budget: Some(budget),
                child_execution: Some(ChildExecutionContext {
                    journal: self.journal.clone(),
                    approver: self.approver.clone(),
                }),
                delta: None,
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
        sync_effects(journal.journal(), &run_id, sink.effects)?;
        Ok((cancelled || cancel.is_cancelled(), dispatched))
    }
}

struct HubGuard {
    hub: Option<Arc<RunHub>>,
    snapshot: RunSnapshot,
    finished: bool,
}

impl HubGuard {
    fn new(hub: Option<Arc<RunHub>>, snapshot: RunSnapshot) -> Self {
        Self {
            hub,
            snapshot,
            finished: false,
        }
    }

    fn update(&mut self, snapshot: RunSnapshot) {
        self.snapshot = snapshot.clone();
        if let Some(hub) = &self.hub {
            hub.update(snapshot);
        }
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        if let Some(hub) = &self.hub {
            hub.finish(self.snapshot.clone());
        }
        self.finished = true;
    }
}

impl Drop for HubGuard {
    fn drop(&mut self) {
        self.finish();
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
    budget: &'a BudgetHandle,
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
        self.effect_mut(effect_id)
            .apply_tool_outcome(outcome, known_tool);
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

fn delta_channel() -> (DeltaSink, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let sink: DeltaSink = Arc::new(move |text: String| {
        let _ = sender.send(text);
    });
    (sink, receiver)
}

async fn await_with_deltas<Fut, T, F>(
    future: Fut,
    deltas: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    on_event: &mut F,
) -> T
where
    Fut: Future<Output = T>,
    F: FnMut(&RunEvent),
{
    tokio::pin!(future);
    let result = loop {
        tokio::select! {
            result = &mut future => break result,
            Some(text) = deltas.recv() => on_event(&RunEvent::ModelDelta { text }),
        }
    };
    while let Ok(text) = deltas.try_recv() {
        on_event(&RunEvent::ModelDelta { text });
    }
    result
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

fn policy_denied_outcome(call: &ToolCallRequest, revision: &str) -> ToolOutcome {
    ToolOutcome {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        content: format!("denied by policy `{revision}`; the tool was not executed"),
        is_error: true,
        outcome_unknown: false,
    }
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

fn mark_cancelled_before_dispatch<F: FnMut(&RunEvent)>(
    sink: &mut BatchSink<'_, F>,
    journal: Option<&dyn DurableTransitionStore>,
) -> Result<(), RunError> {
    let run_id = sink.run_id.clone();
    for record in sink.effects.iter_mut() {
        if matches!(
            record.status,
            EffectStatus::Recorded | EffectStatus::AwaitingApproval | EffectStatus::Authorized
        ) {
            record.set_status(EffectStatus::CancelledBeforeDispatch);
            if let Some(journal) = journal {
                journal
                    .record(&run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                journal
                    .complete_outbox(&record.id)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
        }
    }
    Ok(())
}

fn accept_local(
    state: &mut AgentState,
    proposal: TransitionProposal,
    expected_version: u64,
) -> (Vec<EffectIntent>, Disposition) {
    *state = proposal.next;
    state.state_version = expected_version + 1;
    (proposal.intents, proposal.disposition)
}

fn cancel_staged_effects(
    effects: &mut [EffectRecord],
    journal: Option<&dyn DurableTransitionStore>,
    run_id: &RunId,
) -> Result<(), RunError> {
    for record in effects.iter_mut() {
        if matches!(
            record.status,
            EffectStatus::Recorded | EffectStatus::AwaitingApproval | EffectStatus::Authorized
        ) {
            record.set_status(EffectStatus::CancelledBeforeDispatch);
            if let Some(journal) = journal {
                journal
                    .record(run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                journal
                    .complete_outbox(&record.id)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
        }
    }
    Ok(())
}

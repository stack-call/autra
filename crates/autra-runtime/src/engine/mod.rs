//! Local execution engine: coordinator, effect runner and recovery.
//!
//! It depends on core and execution contracts only; harness assembly belongs
//! to the product facade.

mod approval;
mod executor;
mod policy;
mod recovery;
mod streaming;
mod validator;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use crate::approval::digest_intent;
use crate::child::{ChildHandle, ChildSpec};
use crate::dispatcher::{DispatchResult, Dispatcher, OutboxDispatcher};
use crate::hub::RunHub;
use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, BudgetHandle, BudgetSnapshot, CancelFlag,
    Disposition, EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus, EventKind,
    HistoryItem, ModelCallSpec, RunId, ToolCallRequest, ToolOutcome, TransitionProposal,
};
use autra_execution::{
    Approver, DurableTransitionStore, EffectJournal, ObserverHook, PolicyChain, RunError, RunEvent,
    RunOptions, RunOutcome, RunSnapshot, ToolPolicy, ToolRegistry, UNCLAIMED_CLAIM,
};
use executor::{EffectExecutor, ModelExecutionContext, ModelStep, ToolBatchContext};

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
        let before_initialize = state.clone();
        stepper.initialize(&mut state, &history);
        validator::validate_initialized(&before_initialize, &state)?;
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
                    if let Some(journal) = &self.journal {
                        journal
                            .save_run(&state)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                    }
                    if let Some(observer) = &self.observer {
                        observer.on_run_finished(&run_id, &state.phase);
                    }
                    hub_guard.update(RunSnapshot::from_state(&state, event.seq, effects.len()));
                    hub_guard.finish();
                    return Err(RunError::Step(error));
                }
            };
            let TransitionProposal {
                mut next,
                intents,
                disposition,
            } = transition;
            next.state_version = expected_version + 1;
            if let Err(error) = validator::validate(&state, &next, expected_version) {
                let message = error.to_string();
                state.phase = AgentPhase::Failed;
                state.last_error = Some(message.clone());
                on_event(&RunEvent::RunFailed { message });
                if let Some(journal) = &self.journal {
                    journal
                        .save_run(&state)
                        .map_err(|save_error| RunError::Journal(save_error.to_string()))?;
                }
                if let Some(observer) = &self.observer {
                    observer.on_run_finished(&run_id, &state.phase);
                }
                hub_guard.update(RunSnapshot::from_state(&state, event.seq, effects.len()));
                hub_guard.finish();
                return Err(error);
            }

            let first_new = effects.len();
            let mut tool_calls: Vec<(EffectId, ToolCallRequest)> = Vec::new();
            let mut model_requests: Vec<(EffectId, ModelCallSpec)> = Vec::new();
            for intent in intents {
                if let Some(observer) = &self.observer {
                    observer.on_effect_proposed(&run_id, &intent);
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
                        &next,
                        &effects[first_new..],
                    )
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
            // Apply only after the authoritative transition succeeded. If the
            // commit fails, the in-memory state is left untouched.
            state = next;
            if let Some(observer) = &self.observer {
                for effect in &effects[first_new..] {
                    observer.on_effect_committed(&run_id, effect);
                }
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
                let mut context = ToolBatchContext {
                    on_event: &mut on_event,
                    run_id: &run_id,
                    approver: approver.as_ref(),
                    effects: &mut effects,
                    budget: &budget,
                    observer: self.observer.as_ref(),
                };
                let batch = EffectExecutor::new(self)
                    .execute_tool_batch(
                        tool_calls,
                        parallel_limit,
                        effect_timeout,
                        &options.cancel,
                        &mut context,
                    )
                    .await?;
                for outcome in batch.outcomes {
                    enqueue(&mut queue, &mut seq, EventKind::ToolResult(outcome));
                }
                if batch.cancelled {
                    queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
                }
                state.budget = budget.snapshot();
            }

            for (effect_id, request) in model_requests {
                let mut context = ModelExecutionContext {
                    on_event: &mut on_event,
                    run_id: &run_id,
                    effects: &mut effects,
                    observer: self.observer.as_ref(),
                    cancel: &options.cancel,
                    effect_timeout,
                    turn: state.budget.turns_used,
                };
                match EffectExecutor::new(self)
                    .execute_model(&effect_id, &request, &mut context)
                    .await
                {
                    Ok(ModelStep::Output(output)) => {
                        enqueue(&mut queue, &mut seq, EventKind::ModelResult(output));
                    }
                    Ok(ModelStep::Cancelled) => {
                        queue_cancel(&mut queue, &mut seq, &mut cancel_queued);
                        break;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        state.phase = AgentPhase::Failed;
                        state.last_error = Some(message.clone());
                        on_event(&RunEvent::RunFailed { message });
                        if let Some(journal) = &self.journal {
                            journal
                                .save_run(&state)
                                .map_err(|save_error| RunError::Journal(save_error.to_string()))?;
                        }
                        if let Some(observer) = &self.observer {
                            observer.on_run_finished(&run_id, &state.phase);
                        }
                        hub_guard.update(RunSnapshot::from_state(&state, event.seq, effects.len()));
                        return Err(error);
                    }
                }
            }
        }
    }
}

fn tool_outcome_from_record(record: &EffectRecord) -> Option<ToolOutcome> {
    let call_id = record.call_id.clone()?;
    let name = record.tool_name.clone()?;
    let (content, is_error, outcome_unknown) = match record.status {
        EffectStatus::Committed => (record.result.clone().unwrap_or_default(), false, false),
        EffectStatus::Failed | EffectStatus::Rejected => (
            record
                .error
                .clone()
                .unwrap_or_else(|| "the effect failed".to_owned()),
            true,
            false,
        ),
        EffectStatus::OutcomeUnknown => (
            record
                .error
                .clone()
                .unwrap_or_else(|| "the effect outcome is unknown".to_owned()),
            true,
            true,
        ),
        EffectStatus::CancelledBeforeDispatch => (
            "the effect was cancelled before dispatch".to_owned(),
            true,
            false,
        ),
        _ => return None,
    };
    Some(ToolOutcome {
        call_id,
        name,
        content,
        is_error,
        outcome_unknown,
    })
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
                    .complete_outbox(&record.id, UNCLAIMED_CLAIM)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
        }
    }
    Ok(())
}

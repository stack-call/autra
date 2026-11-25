//! Run coordination: the per-Run event loop, transition commit, authorization
//! and result collection.

mod authorization;
mod result;
mod transition;

use std::collections::VecDeque;
use std::sync::Arc;

use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, BudgetHandle, BudgetSnapshot, Disposition,
    EffectRecord, EventKind, HistoryItem, RunId, TransitionProposal,
};
use autra_execution::{RunError, RunEvent, RunOptions, RunOutcome, RunSnapshot};

use crate::hub::RunHub;

use super::executor::{EffectExecutor, ModelExecutionContext, ModelStep, ToolBatchContext};
use super::lease::RunLeaseGuard;
use super::pool::{self, PoolEvent};
use super::{LocalEngine, cancel_staged_effects, validator};
use result::ResultCollector;
pub(super) use result::{emit_tool_result, enqueue};
use transition::{PreparedEffects, TransitionCommitter};

impl LocalEngine {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_with_identity<F>(
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
        let _lease = RunLeaseGuard::acquire(self.journal.clone(), &run_id, options.cancel.clone())?;
        if let Some(observer) = &self.observer {
            observer.on_run_started(&run_id);
        }
        let mut state = AgentState::new(run_id.clone(), budget);
        state.parent_run_id = parent_run_id;
        state.agent_profile = self.agent_profile.clone();
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

    pub(super) async fn drive<F>(
        &self,
        stepper: Arc<dyn AgentStepper>,
        mut state: AgentState,
        queue: VecDeque<AcceptedEvent>,
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
        let committer = TransitionCommitter::new(self);
        let hub = self.hub.clone();
        let mut forward = on_event;
        let mut on_event = |event: &RunEvent| {
            if let Some(hub) = &hub {
                hub.publish(&run_id, event);
            }
            forward(event);
        };
        let pooled = self.pooled_execution && self.journal.is_some();
        let (pool_events, mut pool_inbox) = tokio::sync::mpsc::unbounded_channel();
        let pool_wake = Arc::new(tokio::sync::Notify::new());
        let _pool = PoolGuard(if pooled {
            Some(pool::spawn(
                self,
                run_id.clone(),
                options.cancel.clone(),
                pool_events,
                pool_wake.clone(),
            ))
        } else {
            None
        });
        let mut results = ResultCollector::new(queue);
        let mut hub_guard = HubGuard::new(
            self.hub.clone(),
            RunSnapshot::from_state(&state, results.seq(), effects.len()),
        );
        loop {
            if options.cancel.is_cancelled() {
                results.cancel();
            }

            let event = match results.pop_front() {
                Some(event) => event,
                None if pooled => {
                    let message = pool_inbox.recv().await.ok_or_else(|| {
                        RunError::Protocol("executor loop ended without a result".to_owned())
                    })?;
                    match message {
                        PoolEvent::Accepted(effect_id, kind) => {
                            results.enqueue(kind);
                            if let Some(journal) = &self.journal {
                                super::sync_effect(
                                    journal.journal(),
                                    &run_id,
                                    &effect_id,
                                    &mut effects,
                                )?;
                            }
                            continue;
                        }
                        PoolEvent::Cancelled(effect_id) => {
                            if let Some(journal) = &self.journal {
                                super::sync_effect(
                                    journal.journal(),
                                    &run_id,
                                    &effect_id,
                                    &mut effects,
                                )?;
                            }
                            results.cancel();
                            continue;
                        }
                        PoolEvent::ModelFailed(message) => {
                            let expected_version = state.state_version;
                            state.phase = AgentPhase::Failed;
                            state.last_error = Some(message.clone());
                            state.state_version = expected_version + 1;
                            on_event(&RunEvent::RunFailed {
                                message: message.clone(),
                            });
                            state = committer
                                .commit_terminal(&run_id, expected_version, state)
                                .await?;
                            if let Some(observer) = &self.observer {
                                observer.on_run_finished(&run_id, &state.phase);
                            }
                            hub_guard.update(RunSnapshot::from_state(
                                &state,
                                results.seq(),
                                effects.len(),
                            ));
                            hub_guard.finish();
                            return Err(RunError::Model(autra_execution::ModelError::new(message)));
                        }
                    }
                }
                None => {
                    return Err(RunError::Protocol(
                        "no accepted event to process".to_owned(),
                    ));
                }
            };
            let expected_version = state.state_version;
            let transition = match stepper.transition(&state, &event) {
                Ok(transition) => transition,
                Err(error) => {
                    let message = error.to_string();
                    state.phase = AgentPhase::Failed;
                    state.last_error = Some(message.clone());
                    state.state_version = expected_version + 1;
                    on_event(&RunEvent::RunFailed {
                        message: message.clone(),
                    });
                    state = committer
                        .commit_terminal(&run_id, expected_version, state)
                        .await?;
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
                engine_delta,
            } = transition;
            next.state_version = expected_version + 1;
            if let Err(error) = committer.validate(
                &state,
                &next,
                expected_version,
                &event,
                disposition,
                &intents,
            ) {
                let message = error.to_string();
                state.phase = AgentPhase::Failed;
                state.last_error = Some(message.clone());
                state.state_version = expected_version + 1;
                on_event(&RunEvent::RunFailed { message });
                state = committer
                    .commit_terminal(&run_id, expected_version, state)
                    .await?;
                if let Some(observer) = &self.observer {
                    observer.on_run_finished(&run_id, &state.phase);
                }
                hub_guard.update(RunSnapshot::from_state(&state, event.seq, effects.len()));
                hub_guard.finish();
                return Err(error);
            }

            let first_new = effects.len();
            let PreparedEffects {
                tool_calls,
                model_requests,
            } = committer.prepare(&run_id, &mut effects, intents);
            state = committer
                .commit(
                    &run_id,
                    expected_version,
                    &event,
                    next,
                    effects[first_new..].to_vec(),
                    engine_delta,
                )
                .await?;
            // The commit returns the state it persisted; the in-memory state is
            // only replaced after that succeeds.
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
                results.cancel();
                continue;
            }

            if pooled {
                // Authorization happens here, not in the executor. Once every
                // staged Effect is Authorized it is in the outbox, and the
                // executor loop picks it up.
                if let Some(journal) = &self.journal {
                    self.resolve_pending_approvals(
                        journal.as_ref(),
                        stepper.as_ref(),
                        &state,
                        &mut effects[first_new..],
                        approver.as_ref(),
                        &mut on_event,
                    )
                    .await?;
                }
                pool_wake.notify_one();
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
                    results.tool_result(outcome);
                }
                if batch.cancelled {
                    results.cancel();
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
                        results.model_result(output);
                    }
                    Ok(ModelStep::Cancelled) => {
                        results.cancel();
                        break;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let current_version = state.state_version;
                        state.phase = AgentPhase::Failed;
                        state.last_error = Some(message.clone());
                        state.state_version = current_version + 1;
                        on_event(&RunEvent::RunFailed { message });
                        state = committer
                            .commit_terminal(&run_id, current_version, state)
                            .await?;
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

/// Aborts the executor loop when the coordinator returns.
struct PoolGuard(Option<tokio::task::JoinHandle<()>>);

impl Drop for PoolGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
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

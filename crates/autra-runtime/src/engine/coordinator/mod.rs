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
use super::{LocalEngine, cancel_staged_effects, validator};
pub(super) use result::{emit_tool_result, enqueue, queue_cancel};
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
        let committer = TransitionCommitter::new(self);
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
                    state.state_version = expected_version + 1;
                    on_event(&RunEvent::RunFailed {
                        message: message.clone(),
                    });
                    committer.commit_terminal(&run_id, expected_version, &state)?;
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
                committer.commit_terminal(&run_id, expected_version, &state)?;
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
            committer.commit(
                &run_id,
                expected_version,
                &event,
                &next,
                &effects[first_new..],
            )?;
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
                        let current_version = state.state_version;
                        state.phase = AgentPhase::Failed;
                        state.last_error = Some(message.clone());
                        state.state_version = current_version + 1;
                        on_event(&RunEvent::RunFailed { message });
                        committer.commit_terminal(&run_id, current_version, &state)?;
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

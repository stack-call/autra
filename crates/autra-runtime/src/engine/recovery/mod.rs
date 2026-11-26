mod reconcile;
mod supervisor;

pub use supervisor::{RecoveryLease, RecoverySupervisor};

use std::collections::VecDeque;
use std::sync::Arc;

use futures_util::{StreamExt, stream};

use autra_core::{AcceptedEvent, AgentPhase, AgentStepper, EventKind, RunId};
use autra_execution::{
    RecoveryReport, RunError, RunEvent, RunOptions, RunOutcome, RunSnapshot, now_unix_ms,
};

use super::LocalEngine;
use super::coordinator::enqueue;
use super::lease::RunLeaseGuard;

impl LocalEngine {
    pub async fn resume<F>(
        &self,
        stepper: Arc<dyn AgentStepper>,
        run_id: &RunId,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        self.resume_with_options(stepper, run_id, options, on_event)
            .await
    }

    #[tracing::instrument(skip_all, fields(run_id = %run_id))]
    pub async fn resume_with_options<F>(
        &self,
        stepper: Arc<dyn AgentStepper>,
        run_id: &RunId,
        options: RunOptions,
        mut on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("resume requires a journal".to_owned()))?;
        let _lease = RunLeaseGuard::acquire(self.journal.clone(), run_id, options.cancel.clone())?;
        if journal
            .cancel_request(run_id)
            .map_err(|error| RunError::Journal(error.to_string()))?
            .is_some()
        {
            options.cancel.cancel();
        }
        let state = journal
            .load_run(run_id)
            .map_err(|error| RunError::Journal(error.to_string()))?
            .ok_or_else(|| {
                RunError::Protocol(format!("run `{run_id}` was not found in the journal"))
            })?;
        let mut effects = journal
            .load(run_id)
            .map_err(|error| RunError::Journal(error.to_string()))?;
        let events = journal
            .load_events(run_id)
            .map_err(|error| RunError::Journal(error.to_string()))?;
        let mut seq = events.iter().map(|event| event.seq).max().unwrap_or(0);
        if let Some(hub) = &self.hub {
            hub.start(RunSnapshot::from_state(&state, seq, effects.len()));
        }

        tracing::info!(%run_id, phase = ?state.phase, "agent run resumed");
        on_event(&RunEvent::RunResumed {
            run_id: run_id.clone(),
        });

        match state.phase {
            AgentPhase::Completed => {
                on_event(&RunEvent::RunCompleted {
                    answer: state.final_answer.clone(),
                });
                if let Some(hub) = &self.hub {
                    hub.finish(RunSnapshot::from_state(&state, seq, effects.len()));
                }
                return Ok(RunOutcome::new(state, effects));
            }
            AgentPhase::Cancelled => {
                on_event(&RunEvent::RunCancelled);
                if let Some(hub) = &self.hub {
                    hub.finish(RunSnapshot::from_state(&state, seq, effects.len()));
                }
                return Ok(RunOutcome::new(state, effects));
            }
            AgentPhase::Failed => {
                if let Some(hub) = &self.hub {
                    hub.finish(RunSnapshot::from_state(&state, seq, effects.len()));
                }
                return Err(RunError::Failed(
                    state
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "run failed".to_owned()),
                ));
            }
            AgentPhase::Ready => {
                if let Some(hub) = &self.hub {
                    hub.finish(RunSnapshot::from_state(&state, seq, effects.len()));
                }
                return Err(RunError::Protocol(
                    "run has no committed progress to resume".to_owned(),
                ));
            }
            AgentPhase::AwaitingTool | AgentPhase::AwaitingModel => {}
        }

        if let (Some(expected), Some(stored)) = (&self.agent_profile, &state.agent_profile) {
            if expected != stored {
                return Err(RunError::Protocol(format!(
                    "run `{run_id}` was created by a different agent profile; refusing to resume"
                )));
            }
        }
        let mut queue: VecDeque<AcceptedEvent> = VecDeque::new();
        let approver = options.approver.clone().or_else(|| self.approver.clone());
        if self.pooled_execution || effects.iter().any(|r| r.result_inbox) {
            if !journal.supports_result_inbox() {
                return Err(RunError::Protocol(
                    "pooled execution requires an atomic result inbox".into(),
                ));
            }
            self.authorization_gate()
                .with_owner(_lease.commit_owner())
                .resolve_pending_approvals(
                    journal.as_ref(),
                    stepper.as_ref(),
                    &state,
                    &mut effects,
                    approver.as_ref(),
                    &mut on_event,
                )
                .await?;
            let pending = journal
                .pending_results(run_id, usize::MAX)
                .map_err(|e| RunError::Journal(e.to_string()))?;
            if state.phase == AgentPhase::AwaitingModel
                && pending.is_empty()
                && effects
                    .iter()
                    .filter(|r| r.kind == autra_core::EffectKind::ModelCall)
                    .all(|r| {
                        matches!(
                            r.status,
                            autra_core::EffectStatus::Committed
                                | autra_core::EffectStatus::Failed
                                | autra_core::EffectStatus::OutcomeUnknown
                                | autra_core::EffectStatus::CancelledBeforeDispatch
                        )
                    })
            {
                enqueue(&mut queue, &mut seq, EventKind::ResumeRequested);
            }
            for call in stepper.pending_tool_calls(&state)? {
                if let Some(r) = effects
                    .iter()
                    .rev()
                    .find(|r| r.call_id.as_deref() == Some(&call.call_id))
                {
                    if !pending.iter().any(|p| p.identity.effect_id == r.id) {
                        if let Some(o) = super::tool_outcome_from_record(r) {
                            enqueue(&mut queue, &mut seq, EventKind::ToolResult(o));
                        }
                    }
                }
            }
            return self
                .clone()
                .with_pooled_execution(true)
                .drive(
                    stepper,
                    state,
                    queue,
                    effects,
                    options,
                    on_event,
                    _lease.commit_owner(),
                )
                .await;
        }
        match state.phase {
            AgentPhase::AwaitingTool => {
                self.authorization_gate()
                    .with_owner(_lease.commit_owner())
                    .resolve_pending_approvals(
                        journal.as_ref(),
                        stepper.as_ref(),
                        &state,
                        &mut effects,
                        approver.as_ref(),
                        &mut on_event,
                    )
                    .await?;
                let outcomes = self
                    .reconcile_tool_effects(
                        stepper.as_ref(),
                        journal.as_ref(),
                        &state,
                        &mut effects,
                        options.effect_timeout.or(self.effect_timeout),
                    )
                    .await?;
                for outcome in outcomes {
                    enqueue(&mut queue, &mut seq, EventKind::ToolResult(outcome));
                }
            }
            AgentPhase::AwaitingModel => {
                self.retire_interrupted_models(journal.as_ref(), &state, &mut effects)?;
                enqueue(&mut queue, &mut seq, EventKind::ResumeRequested);
            }
            _ => unreachable!(),
        }

        self.drive(
            stepper,
            state,
            queue,
            effects,
            options,
            on_event,
            _lease.commit_owner(),
        )
        .await
    }

    /// Resume up to `limit` eligible runs, with bounded concurrency.
    pub async fn recover_once(
        &self,
        stepper: Arc<dyn AgentStepper>,
        limit: usize,
    ) -> Result<RecoveryReport, RunError> {
        self.recover_page(stepper, limit, &mut None).await
    }

    pub(super) async fn recover_page(
        &self,
        stepper: Arc<dyn AgentStepper>,
        limit: usize,
        cursor: &mut Option<RunId>,
    ) -> Result<RecoveryReport, RunError> {
        const PAGE_SIZE: usize = 128;
        const MAX_CONCURRENT: usize = 4;

        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("recovery requires a journal".to_owned()))?;
        let mut report = RecoveryReport::default();
        let mut remaining = limit;

        while remaining > 0 {
            let page = journal
                .list_run_ids_page(cursor.as_ref(), PAGE_SIZE)
                .map_err(|error| RunError::Journal(error.to_string()))?;
            let end_of_index = page.len() < PAGE_SIZE;
            let mut candidates = Vec::new();
            for run_id in page {
                report.scanned += 1;
                *cursor = Some(run_id.clone());
                let Some(state) = journal
                    .load_run(&run_id)
                    .map_err(|error| RunError::Journal(error.to_string()))?
                else {
                    continue;
                };
                if !matches!(
                    state.phase,
                    AgentPhase::AwaitingTool | AgentPhase::AwaitingModel
                ) {
                    continue;
                }
                let lease = journal
                    .run_lease(&run_id)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                if lease.is_some_and(|lease| lease.is_live(now_unix_ms())) {
                    report.skipped += 1;
                    continue;
                }
                candidates.push(run_id);
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }

            let mut resumes = stream::iter(candidates)
                .map(|run_id| {
                    let stepper = stepper.clone();
                    async move {
                        let result = self
                            .resume_with_options(stepper, &run_id, RunOptions::default(), |_| {})
                            .await;
                        (run_id, result)
                    }
                })
                .buffer_unordered(MAX_CONCURRENT);
            while let Some((run_id, result)) = resumes.next().await {
                match result {
                    Ok(_) => report.resumed += 1,
                    Err(error) => {
                        report.failed += 1;
                        tracing::warn!(%run_id, %error, "background recovery failed");
                    }
                }
            }
            if end_of_index && remaining > 0 {
                *cursor = None;
                break;
            }
        }
        Ok(report)
    }
}

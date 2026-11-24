mod reconcile;
mod supervisor;

pub use supervisor::{RecoveryLease, RecoverySupervisor};

use std::collections::VecDeque;
use std::sync::Arc;

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
        let options = options;
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
        let _lease = RunLeaseGuard::acquire(self.journal.clone(), run_id, options.cancel.clone())?;
        let mut queue: VecDeque<AcceptedEvent> = VecDeque::new();
        let approver = options.approver.clone().or_else(|| self.approver.clone());
        match state.phase {
            AgentPhase::AwaitingTool => {
                self.resolve_pending_approvals(
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

        self.drive(stepper, state, queue, effects, options, on_event)
            .await
    }

    /// Resume every run parked at a recoverable checkpoint, up to `limit`.
    ///
    /// Runs that are already terminal or failed are skipped. A failure in one
    /// run does not stop the pass; it is counted in the report and logged.
    pub async fn recover_once(
        &self,
        stepper: Arc<dyn AgentStepper>,
        limit: usize,
    ) -> Result<RecoveryReport, RunError> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("recovery requires a journal".to_owned()))?;
        let run_ids = journal
            .list_run_ids()
            .map_err(|error| RunError::Journal(error.to_string()))?;
        let mut report = RecoveryReport::default();
        for run_id in run_ids.into_iter().take(limit) {
            report.scanned += 1;
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
            if let Some(lease) = journal
                .run_lease(&run_id)
                .map_err(|error| RunError::Journal(error.to_string()))?
            {
                if lease.is_live(now_unix_ms()) {
                    report.skipped += 1;
                    continue;
                }
            }
            match self
                .resume_with_options(stepper.clone(), &run_id, RunOptions::default(), |_| {})
                .await
            {
                Ok(_) => report.resumed += 1,
                Err(error) => {
                    report.failed += 1;
                    tracing::warn!(%run_id, %error, "background recovery failed");
                }
            }
        }
        Ok(report)
    }
}

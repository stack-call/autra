use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, AttemptStatus, EffectClass, EffectId,
    EffectIntent, EffectKind, EffectRecord, EffectStatus, EventKind, RunId, ToolCallRequest,
    ToolCallSpec, ToolOutcome,
};
use autra_execution::{
    DurableTransitionStore, PolicyDecision, ReconciliationReport, RecoveryReport, RunError,
    RunEvent, RunOptions, RunOutcome, RunSnapshot, UNCLAIMED_CLAIM, UnresolvedEffect, now_unix_ms,
};

use crate::approval::digest_intent;
use crate::dispatcher::OutboxDispatcher;

use super::lease::RunLeaseGuard;
use super::{LocalEngine, enqueue};

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

    /// Retry effects whose outcome is unknown when their effect class makes a
    /// retry safe. Reconcile only updates the effect ledger; it does not
    /// rewrite Run history.
    pub async fn reconcile(&self, run_id: &RunId) -> Result<ReconciliationReport, RunError> {
        self.reconcile_with(run_id, true).await
    }

    /// `retry_models = false` leaves model effects unresolved, for hosts that
    /// only want to reconcile tool work.
    pub async fn reconcile_with(
        &self,
        run_id: &RunId,
        retry_models: bool,
    ) -> Result<ReconciliationReport, RunError> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("reconciliation requires a journal".to_owned()))?;
        let _lease =
            RunLeaseGuard::acquire(self.journal.clone(), run_id, autra_core::CancelFlag::new())?;
        let mut effects = journal
            .load(run_id)
            .map_err(|error| RunError::Journal(error.to_string()))?;
        let mut retried = Vec::new();
        let mut unresolved = Vec::new();

        for record in effects
            .iter_mut()
            .filter(|record| record.status == EffectStatus::OutcomeUnknown)
        {
            let mut reason = None;
            let safe = match record.kind {
                EffectKind::ModelCall => {
                    if retry_models {
                        true
                    } else {
                        reason = Some("model reconciliation requires a model".to_owned());
                        false
                    }
                }
                EffectKind::ToolCall => match record
                    .tool_name
                    .as_deref()
                    .and_then(|name| self.tools.get(name))
                {
                    None => {
                        reason = Some("tool is no longer registered".to_owned());
                        false
                    }
                    Some(tool) => {
                        let intent_call = serde_json::from_str::<EffectIntent>(&record.intent_json)
                            .ok()
                            .and_then(EffectIntent::into_tool_call);
                        let denied = intent_call.as_ref().is_some_and(|call| {
                            self.policy_decision(run_id, call) == Some(PolicyDecision::Deny)
                        });
                        if denied {
                            reason = Some("denied by current policy".to_owned());
                            false
                        } else {
                            let class = tool.effect_class();
                            let class_safe =
                                matches!(class, EffectClass::ReadOnly | EffectClass::Idempotent);
                            if !class_safe {
                                reason = Some(match class {
                                    EffectClass::Reconcileable => {
                                        "tool is Reconcileable; automatic retry is not defined"
                                            .to_owned()
                                    }
                                    _ => "tool is not safe to retry automatically".to_owned(),
                                });
                            }
                            let approval_ok = !tool.contract().requires_approval
                                || intent_call.as_ref().is_some_and(|call| {
                                    self.has_valid_approval(run_id, &record.id, call)
                                });
                            if !approval_ok {
                                reason =
                                    Some("approval is no longer valid for this effect".to_owned());
                            }
                            class_safe && approval_ok
                        }
                    }
                },
            };

            if safe && record.intent_json.is_empty() {
                reason = Some("effect has no recorded intent".to_owned());
            }
            let safe = safe && reason.is_none();

            if safe {
                record.note_reconciliation(match record.kind {
                    EffectKind::ModelCall => "reconciliation: re-issuing the model call",
                    EffectKind::ToolCall => "reconciliation: retrying a safe tool effect",
                });
                record.set_status(EffectStatus::Authorized);
                journal
                    .record(run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                journal
                    .enqueue_effect(run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                retried.push(record.id.clone());
            } else {
                unresolved.push(UnresolvedEffect {
                    effect_id: record.id.clone(),
                    reason: reason.unwrap_or_else(|| "not retried".to_owned()),
                });
            }
        }

        if !retried.is_empty() {
            OutboxDispatcher::run_once_for_run(
                journal.journal(),
                run_id,
                retried.len(),
                self.dispatcher.as_ref(),
                self.effect_timeout,
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()))?;
        }

        Ok(ReconciliationReport {
            run_id: run_id.clone(),
            retried,
            unresolved,
        })
    }

    fn retire_interrupted_models(
        &self,
        journal: &dyn DurableTransitionStore,
        state: &AgentState,
        effects: &mut [EffectRecord],
    ) -> Result<(), RunError> {
        for record in effects.iter_mut().filter(|record| {
            record.kind == EffectKind::ModelCall
                && matches!(
                    record.status,
                    EffectStatus::Recorded
                        | EffectStatus::AwaitingApproval
                        | EffectStatus::Authorized
                        | EffectStatus::Dispatched
                )
        }) {
            record.set_status(EffectStatus::OutcomeUnknown);
            record.record_attempt(
                AttemptStatus::OutcomeUnknown,
                None,
                Some("interrupted before the model result was accepted".to_owned()),
            );
            journal
                .record(&state.run_id, record)
                .map_err(|error| RunError::Journal(error.to_string()))?;
            journal
                .complete_outbox(&record.id, UNCLAIMED_CLAIM)
                .map_err(|error| RunError::Journal(error.to_string()))?;
        }
        Ok(())
    }

    async fn reconcile_tool_effects(
        &self,
        stepper: &dyn AgentStepper,
        journal: &dyn DurableTransitionStore,
        state: &AgentState,
        effects: &mut Vec<EffectRecord>,
        effect_timeout: Option<Duration>,
    ) -> Result<Vec<ToolOutcome>, RunError> {
        let run_id = &state.run_id;
        let pending = stepper.pending_tool_calls(state)?;
        if pending.is_empty() {
            return Err(RunError::Protocol(
                "run is awaiting tools but has no pending calls".to_owned(),
            ));
        }

        for call in &pending {
            let has_record = effects.iter().any(|record| {
                record.kind == EffectKind::ToolCall
                    && record.call_id.as_deref() == Some(call.call_id.as_str())
            });
            if has_record {
                continue;
            }
            let intent = EffectIntent::CallTool(ToolCallSpec { call: call.clone() });
            let mut record = EffectRecord::new(
                EffectId::fresh(),
                EffectKind::ToolCall,
                Some(call.name.clone()),
                Some(call.call_id.clone()),
                digest_intent(&intent),
            )
            .with_intent_json(serde_json::to_string(&intent).unwrap_or_default());
            record.set_status(EffectStatus::Failed);
            record.record_attempt(
                AttemptStatus::Failed,
                None,
                Some("effect was not recorded before the interruption".to_owned()),
            );
            journal
                .record(run_id, &record)
                .map_err(|error| RunError::Journal(error.to_string()))?;
            effects.push(record);
        }

        let mut needs_dispatch = 0_usize;
        for call in &pending {
            let Some(record) = effects.iter_mut().find(|record| {
                record.kind == EffectKind::ToolCall
                    && record.call_id.as_deref() == Some(call.call_id.as_str())
            }) else {
                continue;
            };
            match record.status {
                EffectStatus::Authorized => {
                    journal
                        .enqueue_effect(run_id, record)
                        .map_err(|error| RunError::Journal(error.to_string()))?;
                    needs_dispatch += 1;
                }
                EffectStatus::Recorded | EffectStatus::AwaitingApproval => {
                    record.set_status(EffectStatus::Rejected);
                    record.error = Some(
                        "approval was not completed before the interruption; the call was not executed"
                            .to_owned(),
                    );
                    journal
                        .record(run_id, record)
                        .map_err(|error| RunError::Journal(error.to_string()))?;
                }
                EffectStatus::Dispatched => {
                    let retryable = !record.intent_json.is_empty()
                        && record
                            .tool_name
                            .as_deref()
                            .and_then(|name| self.tools.get(name))
                            .map(|tool| {
                                matches!(
                                    tool.effect_class(),
                                    EffectClass::ReadOnly | EffectClass::Idempotent
                                )
                            })
                            .unwrap_or(false);
                    if retryable {
                        record.set_status(EffectStatus::Authorized);
                        record.record_attempt(
                            AttemptStatus::OutcomeUnknown,
                            None,
                            Some(
                                "retrying after an interrupted attempt; the tool is read-only or idempotent"
                                    .to_owned(),
                            ),
                        );
                        journal
                            .record(run_id, record)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                        journal
                            .enqueue_effect(run_id, record)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                        needs_dispatch += 1;
                    } else {
                        record.set_status(EffectStatus::OutcomeUnknown);
                        record.record_attempt(
                            AttemptStatus::OutcomeUnknown,
                            None,
                            Some(
                                "the call was in flight when the process stopped; the outcome is unknown"
                                    .to_owned(),
                            ),
                        );
                        journal
                            .record(run_id, record)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                        journal
                            .complete_outbox(&record.id, UNCLAIMED_CLAIM)
                            .map_err(|error| RunError::Journal(error.to_string()))?;
                    }
                }
                _ => {}
            }
        }

        if needs_dispatch > 0 {
            OutboxDispatcher::run_once_for_run(
                journal.journal(),
                run_id,
                needs_dispatch,
                self.dispatcher.as_ref(),
                effect_timeout,
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()))?;
        }

        *effects = journal
            .load(run_id)
            .map_err(|error| RunError::Journal(error.to_string()))?;

        Ok(pending
            .iter()
            .map(|call| {
                let record = effects.iter().find(|record| {
                    record.kind == EffectKind::ToolCall
                        && record.call_id.as_deref() == Some(call.call_id.as_str())
                });
                tool_outcome_after_resume(call, record)
            })
            .collect())
    }
}

fn tool_outcome_after_resume(call: &ToolCallRequest, record: Option<&EffectRecord>) -> ToolOutcome {
    let (content, is_error, outcome_unknown) = match record {
        None => (
            "effect was not recorded before the interruption".to_owned(),
            true,
            false,
        ),
        Some(record) => match record.status {
            EffectStatus::Committed => (
                record.result.clone().unwrap_or_default(),
                record.error.is_some(),
                false,
            ),
            EffectStatus::Failed => (
                record
                    .error
                    .clone()
                    .or_else(|| record.result.clone())
                    .unwrap_or_else(|| "tool failed".to_owned()),
                true,
                false,
            ),
            EffectStatus::Rejected => (
                record
                    .error
                    .clone()
                    .unwrap_or_else(|| "approval rejected".to_owned()),
                true,
                false,
            ),
            EffectStatus::OutcomeUnknown | EffectStatus::Dispatched => (
                "outcome unknown after resume; the tool may have run".to_owned(),
                true,
                true,
            ),
            EffectStatus::CancelledBeforeDispatch => {
                ("cancelled before dispatch".to_owned(), true, false)
            }
            _ => (
                "effect could not be dispatched on resume".to_owned(),
                true,
                false,
            ),
        },
    };
    ToolOutcome {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        content,
        is_error,
        outcome_unknown,
    }
}

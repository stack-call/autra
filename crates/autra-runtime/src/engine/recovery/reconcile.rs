//! Reconciliation: retry only safe OutcomeUnknown Effects and report the rest.

use std::time::Duration;

use autra_core::{
    AgentState, AgentStepper, AttemptStatus, EffectClass, EffectId, EffectIntent, EffectKind,
    EffectRecord, EffectStatus, RunId, ToolCallRequest, ToolCallSpec, ToolOutcome,
};
use autra_execution::{
    DurableTransitionStore, PolicyDecision, ReconciliationReport, RunError, UNCLAIMED_CLAIM,
    UnresolvedEffect,
};

use crate::approval::digest_intent;
use crate::dispatcher::{DEFAULT_CLAIM_LEASE, DispatchOptions, OutboxDispatcher};

use super::super::LocalEngine;
use super::super::lease::RunLeaseGuard;

impl LocalEngine {
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
                            self.authorization_gate().policy_decision(run_id, call)
                                == Some(PolicyDecision::Deny)
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
                                    self.authorization_gate()
                                        .has_valid_approval(run_id, &record.id, call)
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
            let options = DispatchOptions {
                limit: retried.len(),
                max_parallel: 1,
                timeout: self.effect_timeout,
                cancel: None,
                lease: DEFAULT_CLAIM_LEASE,
                budget: None,
                child_execution: None,
                delta: None,
                scheduler: Some(self.scheduler.clone()),
            };
            OutboxDispatcher::run_once_for_run_with(
                journal.journal(),
                run_id,
                options,
                self.dispatcher.as_ref(),
                |_| {},
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

    pub(super) fn retire_interrupted_models(
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

    pub(super) async fn reconcile_tool_effects(
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
            let options = DispatchOptions {
                limit: needs_dispatch,
                max_parallel: 1,
                timeout: effect_timeout,
                cancel: None,
                lease: DEFAULT_CLAIM_LEASE,
                budget: None,
                child_execution: None,
                delta: None,
                scheduler: Some(self.scheduler.clone()),
            };
            OutboxDispatcher::run_once_for_run_with(
                journal.journal(),
                run_id,
                options,
                self.dispatcher.as_ref(),
                |_| {},
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

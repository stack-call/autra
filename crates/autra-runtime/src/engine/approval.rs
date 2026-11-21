use std::sync::Arc;

use autra_core::{
    AgentState, AgentStepper, EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus,
    RunId, ToolCallRequest,
};
use autra_execution::{
    ApprovalRecord, Approver, DEFAULT_APPROVAL_TTL, DurableTransitionStore, PolicyDecision,
    RunError, RunEvent, now_unix_ms,
};

use crate::approval::{ApprovalDecision, ApprovalRequest};

use super::LocalEngine;

impl LocalEngine {
    pub(super) fn approval_request(
        &self,
        run_id: &RunId,
        effect_id: &EffectId,
        call: &ToolCallRequest,
    ) -> ApprovalRequest {
        let preview = self
            .tools
            .get(&call.name)
            .and_then(|tool| tool.preview(&call.arguments));
        ApprovalRequest::new(run_id, effect_id, call)
            .with_policy(
                self.policy.revision(),
                self.actor.clone(),
                Some(self.approval_ttl.unwrap_or(DEFAULT_APPROVAL_TTL)),
            )
            .with_preview(preview)
    }

    /// Reuse a matching persisted decision, otherwise ask the approver and
    /// persist the new decision.
    pub(super) async fn resolve_pending_approvals<F>(
        &self,
        journal: &dyn DurableTransitionStore,
        stepper: &dyn AgentStepper,
        state: &AgentState,
        effects: &mut [EffectRecord],
        approver: Option<&Arc<dyn Approver>>,
        on_event: &mut F,
    ) -> Result<(), RunError>
    where
        F: FnMut(&RunEvent),
    {
        for call in stepper.pending_tool_calls(state)? {
            let Some(record) = effects.iter_mut().find(|record| {
                record.kind == EffectKind::ToolCall
                    && record.call_id.as_deref() == Some(call.call_id.as_str())
            }) else {
                continue;
            };
            if !matches!(
                record.status,
                EffectStatus::Recorded | EffectStatus::AwaitingApproval
            ) {
                continue;
            }
            let intent_call = serde_json::from_str::<EffectIntent>(&record.intent_json)
                .ok()
                .and_then(EffectIntent::into_tool_call)
                .unwrap_or(call);
            match self.policy_decision(&state.run_id, &intent_call) {
                Some(PolicyDecision::Deny) => {
                    record.set_status(EffectStatus::Rejected);
                    record.error = Some(format!(
                        "denied by policy `{}`; the tool was not executed",
                        self.policy.revision()
                    ));
                    journal
                        .record(&state.run_id, record)
                        .map_err(|error| RunError::Journal(error.to_string()))?;
                    continue;
                }
                Some(PolicyDecision::Allow) => {
                    record.set_status(EffectStatus::Authorized);
                    journal
                        .enqueue_effect(&state.run_id, record)
                        .map_err(|error| RunError::Journal(error.to_string()))?;
                    journal
                        .record(&state.run_id, record)
                        .map_err(|error| RunError::Journal(error.to_string()))?;
                    continue;
                }
                _ => {}
            }
            let request = self.approval_request(&state.run_id, &record.id, &intent_call);
            on_event(&RunEvent::ApprovalRequested {
                call_id: request.call_id.clone(),
                tool_name: request.tool_name.clone(),
                argument_digest: request.argument_digest.clone(),
            });
            let decision = self
                .resolve_approval(&request, &record.id, approver)
                .await?;
            let approved = decision == ApprovalDecision::Approved;
            on_event(&RunEvent::ApprovalDecided {
                call_id: request.call_id.clone(),
                approved,
            });
            if approved {
                record.set_status(EffectStatus::Authorized);
                journal
                    .enqueue_effect(&state.run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            } else {
                record.set_status(EffectStatus::Rejected);
                record.error = Some("approval rejected; the tool was not executed".to_owned());
            }
            journal
                .record(&state.run_id, record)
                .map_err(|error| RunError::Journal(error.to_string()))?;
        }
        Ok(())
    }

    pub(super) async fn resolve_approval(
        &self,
        request: &ApprovalRequest,
        effect_id: &EffectId,
        approver: Option<&Arc<dyn Approver>>,
    ) -> Result<ApprovalDecision, RunError> {
        let store = self
            .journal
            .as_ref()
            .map(|journal| journal.approval_store());
        if let Some(store) = store {
            match store.load_approval(effect_id) {
                Ok(Some(record)) if record.is_usable_for(request, now_unix_ms()) => {
                    return Ok(record.decision);
                }
                Ok(_) => {}
                Err(error) => return Err(RunError::Journal(error.to_string())),
            }
        }
        let decision = match approver {
            Some(approver) => approver.approve(request).await,
            None => ApprovalDecision::Rejected,
        };
        if let Some(store) = store {
            let record = ApprovalRecord::from_request(request, decision, now_unix_ms());
            store
                .save_approval(&record)
                .map_err(|error| RunError::Journal(error.to_string()))?;
        }
        Ok(decision)
    }

    pub(super) fn has_valid_approval(
        &self,
        run_id: &RunId,
        effect_id: &EffectId,
        call: &ToolCallRequest,
    ) -> bool {
        let Some(journal) = &self.journal else {
            return false;
        };
        let Ok(Some(record)) = journal.approval_store().load_approval(effect_id) else {
            return false;
        };
        let request = ApprovalRequest::new(run_id, effect_id, call).with_policy(
            self.policy.revision(),
            self.actor.clone(),
            Some(self.approval_ttl.unwrap_or(DEFAULT_APPROVAL_TTL)),
        );
        record.decision == ApprovalDecision::Approved
            && record.is_usable_for(&request, now_unix_ms())
    }
}

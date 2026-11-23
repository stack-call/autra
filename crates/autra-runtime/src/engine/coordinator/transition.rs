//! Transition commit: validate a proposal, materialize its Effects, and
//! persist the state/event/effect/outbox tuple before the caller applies it.

use autra_core::{
    AcceptedEvent, AgentState, Disposition, EffectId, EffectIntent, EffectKind, EffectRecord,
    EffectStatus, ModelCallSpec, RunId, ToolCallRequest,
};
use autra_execution::RunError;

use crate::approval::digest_intent;

use super::super::{LocalEngine, validator};

pub(super) struct PreparedEffects {
    pub tool_calls: Vec<(EffectId, ToolCallRequest)>,
    pub model_requests: Vec<(EffectId, ModelCallSpec)>,
}

pub(super) struct TransitionCommitter<'a> {
    engine: &'a LocalEngine,
}

impl<'a> TransitionCommitter<'a> {
    pub(super) fn new(engine: &'a LocalEngine) -> Self {
        Self { engine }
    }

    pub(super) fn validate(
        &self,
        current: &AgentState,
        next: &AgentState,
        expected_version: u64,
        event: &AcceptedEvent,
        disposition: Disposition,
        intents: &[EffectIntent],
    ) -> Result<(), RunError> {
        validator::validate(current, next, expected_version, event, disposition, intents)
    }

    /// Materialize the proposal's intents as Effect records. Approval-required
    /// tools start as `AwaitingApproval`; everything else starts Authorized.
    pub(super) fn prepare(
        &self,
        run_id: &RunId,
        effects: &mut Vec<EffectRecord>,
        intents: Vec<EffectIntent>,
    ) -> PreparedEffects {
        let mut tool_calls = Vec::new();
        let mut model_requests = Vec::new();
        for intent in intents {
            if let Some(observer) = &self.engine.observer {
                observer.on_effect_proposed(run_id, &intent);
            }
            let effect_id = EffectId::fresh();
            let digest = digest_intent(&intent);
            let intent_json = serde_json::to_string(&intent).unwrap_or_default();
            let requires_approval = match &intent {
                EffectIntent::CallModel(_) => false,
                EffectIntent::CallTool(spec) => self.engine.tool_needs_gate(run_id, &spec.call),
            };
            let mut record = match &intent {
                EffectIntent::CallModel(_) => {
                    EffectRecord::new(effect_id.clone(), EffectKind::ModelCall, None, None, digest)
                }
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
        PreparedEffects {
            tool_calls,
            model_requests,
        }
    }

    /// Commit the transition as one durable fact. The caller applies `next`
    /// only after this returns `Ok`.
    pub(super) fn commit(
        &self,
        run_id: &RunId,
        expected_version: u64,
        event: &AcceptedEvent,
        next: &AgentState,
        new_effects: &[EffectRecord],
    ) -> Result<(), RunError> {
        if let Some(journal) = &self.engine.journal {
            journal
                .commit_transition(run_id, expected_version, event, next, new_effects)
                .map_err(|error| RunError::Journal(error.to_string()))?;
        }
        Ok(())
    }

    /// Commit a terminal state produced outside a stepper transition.
    pub(super) fn commit_terminal(
        &self,
        run_id: &RunId,
        expected_version: u64,
        state: &AgentState,
    ) -> Result<(), RunError> {
        if let Some(journal) = &self.engine.journal {
            journal
                .commit_terminal(run_id, expected_version, state)
                .map_err(|error| RunError::Journal(error.to_string()))?;
        }
        Ok(())
    }
}

//! Transition commit: validate a proposal, materialize its Effects, and
//! persist the state/event/effect/outbox tuple before the caller applies it.

use autra_core::{
    AcceptedEvent, AgentState, Disposition, EffectId, EffectIntent, EffectKind, EffectRecord,
    EffectStatus, EngineStateDelta, ModelCallSpec, RunId, ToolCallRequest,
};
use autra_execution::RunError;

use crate::approval::digest_intent;
use crate::blocking::run_journal_with;

use super::super::{LocalEngine, validator};

pub(super) struct PreparedEffects {
    pub tool_calls: Vec<(EffectId, ToolCallRequest)>,
    pub model_requests: Vec<(EffectId, ModelCallSpec)>,
}

pub(super) struct TransitionCommitter<'a> {
    engine: &'a LocalEngine,
    owner_id: Option<String>,
}

impl<'a> TransitionCommitter<'a> {
    pub(super) fn new(engine: &'a LocalEngine, owner_id: Option<String>) -> Self {
        Self { engine, owner_id }
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
                EffectIntent::CallTool(spec) => self
                    .engine
                    .authorization_gate()
                    .tool_needs_gate(run_id, &spec.call),
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
            if let EffectIntent::CallTool(spec) = &intent {
                if let Some(tool) = self.engine.tools.get(&spec.call.name) {
                    record.requirements = tool.execution_requirements();
                }
            }
            record.result_inbox = self.engine.pooled_execution && self.engine.journal.is_some();
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

    /// Commit the transition as one durable fact on the blocking pool. The
    /// caller applies the returned state only after this succeeds.
    pub(super) async fn commit(
        &self,
        expected_version: u64,
        event: &AcceptedEvent,
        next: AgentState,
        new_effects: Vec<EffectRecord>,
        delta: Option<EngineStateDelta>,
        source: Option<autra_execution::ResultIdentity>,
    ) -> Result<AgentState, RunError> {
        let Some(journal) = self.engine.journal.clone() else {
            return Ok(next);
        };
        if let Some(delta) = &delta {
            if delta.base_version != expected_version {
                return Err(RunError::Protocol(format!(
                    "engine delta base_version {} does not match expected {expected_version}",
                    delta.base_version
                )));
            }
        }
        let run_id = next.run_id.clone();
        let owner_id = self.owner_id.clone();
        let event = event.clone();
        let wake = new_effects
            .iter()
            .any(|effect| effect.status == EffectStatus::Authorized)
            .then(|| self.engine.run_contexts.resolve(&run_id))
            .flatten()
            .map(|context| context.work_ready.clone());
        let committed = run_journal_with(move || {
            let result = match owner_id {
                Some(owner_id) => journal.commit_owned_transition(
                    &run_id,
                    expected_version,
                    &event,
                    &next,
                    &new_effects,
                    delta.as_ref(),
                    &autra_execution::CommitAuthority {
                        owner_id,
                        result: source,
                    },
                ),
                None => journal.commit_transition_with_engine_delta(
                    &run_id,
                    expected_version,
                    &event,
                    &next,
                    &new_effects,
                    delta.as_ref(),
                ),
            };
            (next, result)
        })
        .await?;
        if let Some(wake) = wake {
            wake.notify_one();
        }
        Ok(committed)
    }

    /// Commit a terminal state produced outside a stepper transition.
    pub(super) async fn commit_terminal(
        &self,
        run_id: &RunId,
        expected_version: u64,
        state: AgentState,
        source: Option<autra_execution::ResultIdentity>,
    ) -> Result<AgentState, RunError> {
        let Some(journal) = self.engine.journal.clone() else {
            return Ok(state);
        };
        let run_id = run_id.clone();
        let owner_id = self.owner_id.clone();
        run_journal_with(move || {
            let result = match owner_id {
                Some(owner_id) => journal.commit_owned_terminal(
                    &run_id,
                    expected_version,
                    &state,
                    &autra_execution::CommitAuthority {
                        owner_id,
                        result: source,
                    },
                ),
                None => journal.commit_terminal(&run_id, expected_version, &state),
            };
            (state, result)
        })
        .await
    }
}

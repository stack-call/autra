//! Effect journal boundary. A durable host persists EffectRecords here.

use std::time::Duration;

use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, RunId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("{0}")]
    Message(String),
    #[error("claim `{claim_id}` no longer owns effect `{effect_id}`")]
    ClaimLost { effect_id: String, claim_id: String },
}

impl JournalError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    pub fn claim_lost(effect_id: &EffectId, claim_id: &str) -> Self {
        Self::ClaimLost {
            effect_id: effect_id.to_string(),
            claim_id: claim_id.to_owned(),
        }
    }

    pub fn is_claim_lost(&self) -> bool {
        matches!(self, Self::ClaimLost { .. })
    }
}

/// Ownership of a Run or a local service. Deadlines are Unix milliseconds so
/// another process can decide whether the owner is still alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseRecord {
    pub owner_id: String,
    pub deadline_unix_ms: u64,
}

impl LeaseRecord {
    pub fn is_live(&self, now_unix_ms: u64) -> bool {
        self.deadline_unix_ms > now_unix_ms
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEntry {
    pub effect_id: EffectId,
    pub run_id: RunId,
    pub intent_json: String,
    pub attempts: u32,
}

pub trait EffectJournal: Send + Sync {
    /// Unfenced write for pre-claim state such as a committed transition.
    /// Dispatchers must use `record_claimed`.
    fn record(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError>;
    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError>;

    /// Load one Effect without materializing the whole Run ledger.
    fn load_effect(
        &self,
        run_id: &RunId,
        effect_id: &EffectId,
    ) -> Result<Option<EffectRecord>, JournalError> {
        Ok(self
            .load(run_id)?
            .into_iter()
            .find(|record| &record.id == effect_id))
    }

    /// Load only the requested Effects.
    fn load_effects(
        &self,
        run_id: &RunId,
        effect_ids: &[EffectId],
    ) -> Result<Vec<EffectRecord>, JournalError> {
        let all = self.load(run_id)?;
        Ok(all
            .into_iter()
            .filter(|record| effect_ids.iter().any(|id| id == &record.id))
            .collect())
    }

    /// Write an effect only while `claim_id` still owns its outbox row.
    /// The default refuses the write: a journal that cannot fence must not
    /// silently allow a stale owner to record an outcome.
    fn record_claimed(
        &self,
        _run_id: &RunId,
        _effect: &EffectRecord,
        _claim_id: &str,
    ) -> Result<(), JournalError> {
        Err(JournalError::new(
            "journal does not support claim-fenced writes",
        ))
    }

    /// Extend a claim that has not expired. `false` means the claim was lost.
    fn renew_claim(
        &self,
        _effect_id: &EffectId,
        _claim_id: &str,
        _lease: Duration,
    ) -> Result<bool, JournalError> {
        Ok(false)
    }

    /// Acquire exclusive ownership of a Run.
    fn claim_run(
        &self,
        _run_id: &RunId,
        _owner_id: &str,
        _lease: Duration,
    ) -> Result<bool, JournalError> {
        Ok(true)
    }

    /// Refresh a Run lease. `false` means another owner took it over.
    fn renew_run(
        &self,
        _run_id: &RunId,
        _owner_id: &str,
        _lease: Duration,
    ) -> Result<bool, JournalError> {
        Ok(true)
    }

    fn release_run(&self, _run_id: &RunId, _owner_id: &str) -> Result<(), JournalError> {
        Ok(())
    }

    fn run_lease(&self, _run_id: &RunId) -> Result<Option<LeaseRecord>, JournalError> {
        Ok(None)
    }

    /// Acquire exclusive ownership of a local service such as the recovery
    /// daemon. Uses the same lease model as a Run.
    fn claim_service(
        &self,
        _service: &str,
        _owner_id: &str,
        _lease: Duration,
    ) -> Result<bool, JournalError> {
        Ok(true)
    }

    fn renew_service(
        &self,
        _service: &str,
        _owner_id: &str,
        _lease: Duration,
    ) -> Result<bool, JournalError> {
        Ok(true)
    }

    fn release_service(&self, _service: &str, _owner_id: &str) -> Result<(), JournalError> {
        Ok(())
    }

    /// Persist an operator's cancellation intent. This is separate from the
    /// accepted CancelRequested event so a killed process can still be seen as
    /// cancelled by recovery.
    fn request_cancel(&self, _run_id: &RunId, _reason: &str) -> Result<(), JournalError> {
        Ok(())
    }

    fn cancel_request(&self, _run_id: &RunId) -> Result<Option<String>, JournalError> {
        Ok(None)
    }

    fn clear_cancel(&self, _run_id: &RunId) -> Result<(), JournalError> {
        Ok(())
    }

    fn save_run(&self, _state: &AgentState) -> Result<(), JournalError> {
        Ok(())
    }

    fn load_run(&self, _run_id: &RunId) -> Result<Option<AgentState>, JournalError> {
        Ok(None)
    }

    /// Run ids that may need recovery. Hosts without a run index may return an
    /// empty list.
    fn list_run_ids(&self) -> Result<Vec<RunId>, JournalError> {
        Ok(Vec::new())
    }

    fn commit_event(
        &self,
        run_id: &RunId,
        _event: &AcceptedEvent,
        state: &AgentState,
        effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        self.save_run(state)?;
        for effect in effects {
            self.record(run_id, effect)?;
        }
        Ok(())
    }

    fn load_events(&self, _run_id: &RunId) -> Result<Vec<AcceptedEvent>, JournalError> {
        Ok(Vec::new())
    }
    fn enqueue_effect(&self, _run_id: &RunId, _effect: &EffectRecord) -> Result<(), JournalError> {
        Ok(())
    }

    fn claim_due_effects(
        &self,
        _limit: usize,
        _claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(Vec::new())
    }

    fn claim_due_effects_for_run(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let _ = run_id;
        self.claim_due_effects(limit, claim_id)
    }

    /// Claim with an explicit lease. Hosts that track claims may reclaim a row
    /// after the lease expires.
    fn claim_due_effects_with_lease(
        &self,
        limit: usize,
        claim_id: &str,
        _lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        self.claim_due_effects(limit, claim_id)
    }

    fn claim_due_effects_for_run_with_lease(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
        _lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        self.claim_due_effects_for_run(run_id, limit, claim_id)
    }

    fn complete_outbox(&self, _effect_id: &EffectId, _claim_id: &str) -> Result<(), JournalError> {
        Ok(())
    }

    fn release_outbox(&self, _effect_id: &EffectId, _claim_id: &str) -> Result<(), JournalError> {
        Ok(())
    }
}

/// Default lease for a claimed outbox row.
pub const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(60);

/// Claim id for completing a row that was never claimed by a dispatcher.
pub const UNCLAIMED_CLAIM: &str = "";

/// A journal that can commit a transition atomically with its effects.
///
/// This is a separate contract on purpose: a plain `EffectJournal` may be
/// in-memory or best-effort, and must not silently claim durable semantics.
pub trait DurableTransitionStore: EffectJournal {
    /// The underlying journal, used for dispatch, recovery and inspection.
    fn journal(&self) -> &dyn EffectJournal;

    /// Persist and reuse approval decisions for this Run's effects.
    fn approval_store(&self) -> &dyn crate::approval::ApprovalStore;

    /// Commit the next state, one accepted event, the effects it introduced and
    /// the outbox rows for its `Authorized` effects as one unit.
    fn commit_transition(
        &self,
        run_id: &RunId,
        expected_version: u64,
        event: &AcceptedEvent,
        state: &AgentState,
        new_effects: &[EffectRecord],
    ) -> Result<(), JournalError>;

    /// Commit a terminal state without introducing a new accepted event.
    ///
    /// Used for runtime failures that happen outside a stepper transition, such
    /// as a model transport error. Hosts that support optimistic concurrency
    /// must reject a stale `expected_version` instead of overwriting state.
    fn commit_terminal(
        &self,
        _run_id: &RunId,
        _expected_version: u64,
        state: &AgentState,
    ) -> Result<(), JournalError> {
        self.save_run(state)
    }
}

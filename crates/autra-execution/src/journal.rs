//! Effect journal boundary. A durable host persists EffectRecords here.

use std::time::Duration;

use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, RunId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("{0}")]
    Message(String),
}

impl JournalError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::Message(message.into())
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
    fn record(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError>;
    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError>;

    fn save_run(&self, _state: &AgentState) -> Result<(), JournalError> {
        Ok(())
    }

    fn load_run(&self, _run_id: &RunId) -> Result<Option<AgentState>, JournalError> {
        Ok(None)
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
pub const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(30);

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
}

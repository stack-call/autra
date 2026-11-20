//! Effect journal boundary. A durable host persists EffectRecords here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, EffectStatus, RunId};
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

    fn complete_outbox(&self, _effect_id: &EffectId) -> Result<(), JournalError> {
        Ok(())
    }

    fn release_outbox(&self, _effect_id: &EffectId) -> Result<(), JournalError> {
        Ok(())
    }
}

/// In-process journal for tests and local runs.
#[derive(Clone, Default)]
pub struct MemoryJournal {
    records: Arc<Mutex<BTreeMap<RunId, Vec<EffectRecord>>>>,
    runs: Arc<Mutex<BTreeMap<RunId, AgentState>>>,
    events: Arc<Mutex<BTreeMap<RunId, Vec<AcceptedEvent>>>>,
    outbox: Arc<Mutex<BTreeMap<EffectId, OutboxEntry>>>,
}

impl MemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EffectJournal for MemoryJournal {
    fn record(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        let mut records = self.records.lock().expect("journal lock poisoned");
        let entry = records.entry(run_id.clone()).or_default();
        if let Some(existing) = entry.iter_mut().find(|record| record.id == effect.id) {
            *existing = effect.clone();
        } else {
            entry.push(effect.clone());
        }
        Ok(())
    }

    fn save_run(&self, state: &AgentState) -> Result<(), JournalError> {
        self.runs
            .lock()
            .expect("journal lock poisoned")
            .insert(state.run_id.clone(), state.clone());
        Ok(())
    }

    fn load_run(&self, run_id: &RunId) -> Result<Option<AgentState>, JournalError> {
        Ok(self
            .runs
            .lock()
            .expect("journal lock poisoned")
            .get(run_id)
            .cloned())
    }

    fn commit_event(
        &self,
        run_id: &RunId,
        event: &AcceptedEvent,
        state: &AgentState,
        effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        self.save_run(state)?;
        for effect in effects {
            self.record(run_id, effect)?;
        }
        let mut events = self.events.lock().expect("journal lock poisoned");
        let entry = events.entry(run_id.clone()).or_default();
        if !entry.iter().any(|accepted| accepted.seq == event.seq) {
            entry.push(event.clone());
        }
        Ok(())
    }

    fn load_events(&self, run_id: &RunId) -> Result<Vec<AcceptedEvent>, JournalError> {
        Ok(self
            .events
            .lock()
            .expect("journal lock poisoned")
            .get(run_id)
            .cloned()
            .unwrap_or_default())
    }

    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError> {
        Ok(self
            .records
            .lock()
            .expect("journal lock poisoned")
            .get(run_id)
            .cloned()
            .unwrap_or_default())
    }

    fn enqueue_effect(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        if effect.status != EffectStatus::Authorized || effect.intent_json.is_empty() {
            return Ok(());
        }
        self.outbox
            .lock()
            .expect("journal lock poisoned")
            .entry(effect.id.clone())
            .or_insert_with(|| OutboxEntry {
                effect_id: effect.id.clone(),
                run_id: run_id.clone(),
                intent_json: effect.intent_json.clone(),
                attempts: 0,
            });
        Ok(())
    }

    fn claim_due_effects(
        &self,
        limit: usize,
        _claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let mut outbox = self.outbox.lock().expect("journal lock poisoned");
        let mut claimed = Vec::new();
        for entry in outbox.values_mut().take(limit) {
            entry.attempts += 1;
            claimed.push(entry.clone());
        }
        Ok(claimed)
    }

    fn complete_outbox(&self, effect_id: &EffectId) -> Result<(), JournalError> {
        self.outbox
            .lock()
            .expect("journal lock poisoned")
            .remove(effect_id);
        Ok(())
    }

    fn release_outbox(&self, _effect_id: &EffectId) -> Result<(), JournalError> {
        Ok(())
    }
}

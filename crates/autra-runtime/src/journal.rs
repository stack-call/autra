//! In-process EffectJournal for local runs and tests.

pub use autra_execution::journal::{DEFAULT_CLAIM_LEASE, EffectJournal, JournalError, OutboxEntry};

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, EffectStatus, RunId};

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

    fn claim_due_effects_for_run(
        &self,
        run_id: &RunId,
        limit: usize,
        _claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let mut outbox = self.outbox.lock().expect("journal lock poisoned");
        let mut claimed = Vec::new();
        for entry in outbox
            .values_mut()
            .filter(|entry| &entry.run_id == run_id)
            .take(limit)
        {
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

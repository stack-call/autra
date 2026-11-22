//! In-process EffectJournal for local runs and tests.

pub use autra_execution::journal::{DEFAULT_CLAIM_LEASE, EffectJournal, JournalError, OutboxEntry};
use autra_execution::{ApprovalRecord, ApprovalStore, ApprovalStoreError, DurableTransitionStore};

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
    claims: Arc<Mutex<BTreeMap<EffectId, String>>>,
    approvals: Arc<Mutex<BTreeMap<EffectId, ApprovalRecord>>>,
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

    fn list_run_ids(&self) -> Result<Vec<RunId>, JournalError> {
        Ok(self
            .runs
            .lock()
            .expect("journal lock poisoned")
            .keys()
            .cloned()
            .collect())
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
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let mut outbox = self.outbox.lock().expect("journal lock poisoned");
        let mut claims = self.claims.lock().expect("journal lock poisoned");
        let ids: Vec<EffectId> = outbox
            .values()
            .take(limit)
            .map(|entry| entry.effect_id.clone())
            .collect();
        let mut claimed = Vec::new();
        for id in ids {
            let entry = outbox.get_mut(&id).expect("outbox entry");
            entry.attempts += 1;
            claims.insert(id.clone(), claim_id.to_owned());
            claimed.push(entry.clone());
        }
        Ok(claimed)
    }

    fn claim_due_effects_for_run(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let mut outbox = self.outbox.lock().expect("journal lock poisoned");
        let mut claims = self.claims.lock().expect("journal lock poisoned");
        let ids: Vec<EffectId> = outbox
            .values()
            .filter(|entry| &entry.run_id == run_id)
            .take(limit)
            .map(|entry| entry.effect_id.clone())
            .collect();
        let mut claimed = Vec::new();
        for id in ids {
            let entry = outbox.get_mut(&id).expect("outbox entry");
            entry.attempts += 1;
            claims.insert(id.clone(), claim_id.to_owned());
            claimed.push(entry.clone());
        }
        Ok(claimed)
    }

    fn complete_outbox(&self, effect_id: &EffectId, claim_id: &str) -> Result<(), JournalError> {
        let mut claims = self.claims.lock().expect("journal lock poisoned");
        let owns = match claims.get(effect_id) {
            Some(owner) => owner == claim_id,
            None => claim_id.is_empty(),
        };
        if owns {
            claims.remove(effect_id);
            self.outbox
                .lock()
                .expect("journal lock poisoned")
                .remove(effect_id);
        }
        Ok(())
    }

    fn release_outbox(&self, effect_id: &EffectId, claim_id: &str) -> Result<(), JournalError> {
        let mut claims = self.claims.lock().expect("journal lock poisoned");
        let owns = match claims.get(effect_id) {
            Some(owner) => owner == claim_id,
            None => claim_id.is_empty(),
        };
        if owns {
            claims.remove(effect_id);
        }
        Ok(())
    }
}

impl ApprovalStore for MemoryJournal {
    fn save_approval(&self, record: &ApprovalRecord) -> Result<(), ApprovalStoreError> {
        self.approvals
            .lock()
            .expect("journal lock poisoned")
            .insert(record.effect_id.clone(), record.clone());
        Ok(())
    }

    fn load_approval(
        &self,
        effect_id: &EffectId,
    ) -> Result<Option<ApprovalRecord>, ApprovalStoreError> {
        Ok(self
            .approvals
            .lock()
            .expect("journal lock poisoned")
            .get(effect_id)
            .cloned())
    }
}

impl DurableTransitionStore for MemoryJournal {
    fn journal(&self) -> &dyn EffectJournal {
        self
    }

    fn approval_store(&self) -> &dyn ApprovalStore {
        self
    }

    fn commit_transition(
        &self,
        run_id: &RunId,
        expected_version: u64,
        event: &AcceptedEvent,
        state: &AgentState,
        new_effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        let current = self
            .load_run(run_id)?
            .map_or(0, |stored| stored.state_version);
        if current != expected_version {
            return Err(JournalError::new(format!(
                "state version conflict for run `{run_id}`: expected {expected_version}, found {current}"
            )));
        }
        if state.state_version != expected_version + 1 {
            return Err(JournalError::new(format!(
                "new state version {} does not follow {expected_version}",
                state.state_version
            )));
        }
        if self
            .load_events(run_id)?
            .iter()
            .any(|committed| committed.seq == event.seq)
        {
            return Err(JournalError::new(format!(
                "event seq {} was already committed for run `{run_id}`",
                event.seq
            )));
        }
        self.commit_event(run_id, event, state, new_effects)?;
        for effect in new_effects {
            self.enqueue_effect(run_id, effect)?;
        }
        Ok(())
    }

    fn commit_terminal(
        &self,
        run_id: &RunId,
        expected_version: u64,
        state: &AgentState,
    ) -> Result<(), JournalError> {
        if state.state_version != expected_version + 1 {
            return Err(JournalError::new(format!(
                "terminal state version {} does not follow {expected_version}",
                state.state_version
            )));
        }
        let mut runs = self.runs.lock().expect("journal lock poisoned");
        let current = runs.get(run_id).map_or(0, |state| state.state_version);
        if current != expected_version {
            return Err(JournalError::new(format!(
                "state version conflict for run `{run_id}`: expected {expected_version}, found {current}"
            )));
        }
        runs.insert(run_id.clone(), state.clone());
        Ok(())
    }
}

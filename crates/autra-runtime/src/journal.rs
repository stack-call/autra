//! In-process EffectJournal for local runs and tests.

pub use autra_execution::journal::{DEFAULT_CLAIM_LEASE, EffectJournal, JournalError, OutboxEntry};
use autra_execution::{
    ApprovalRecord, ApprovalStore, ApprovalStoreError, DurableTransitionStore, LeaseRecord,
    now_unix_ms,
};

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, EffectStatus, RunId};

#[derive(Clone, Debug)]
struct MemoryClaim {
    claim_id: String,
    deadline_unix_ms: u64,
}

/// In-process journal for tests and local runs.
#[derive(Clone, Default)]
pub struct MemoryJournal {
    records: Arc<Mutex<BTreeMap<RunId, Vec<EffectRecord>>>>,
    runs: Arc<Mutex<BTreeMap<RunId, AgentState>>>,
    events: Arc<Mutex<BTreeMap<RunId, Vec<AcceptedEvent>>>>,
    outbox: Arc<Mutex<BTreeMap<EffectId, OutboxEntry>>>,
    claims: Arc<Mutex<BTreeMap<EffectId, MemoryClaim>>>,
    run_leases: Arc<Mutex<BTreeMap<RunId, LeaseRecord>>>,
    service_leases: Arc<Mutex<BTreeMap<String, LeaseRecord>>>,
    approvals: Arc<Mutex<BTreeMap<EffectId, ApprovalRecord>>>,
}

fn lease_deadline(lease: Duration) -> u64 {
    now_unix_ms().saturating_add(lease.as_millis() as u64)
}

impl MemoryJournal {
    fn claim_with_lease(
        &self,
        run_id: Option<&RunId>,
        limit: usize,
        claim_id: &str,
        lease: Duration,
    ) -> Vec<OutboxEntry> {
        let now = now_unix_ms();
        let mut outbox = self.outbox.lock().expect("journal lock poisoned");
        let mut claims = self.claims.lock().expect("journal lock poisoned");
        claims.retain(|_, claim| claim.deadline_unix_ms > now);
        let ids: Vec<EffectId> = outbox
            .values()
            .filter(|entry| run_id.is_none_or(|run_id| &entry.run_id == run_id))
            .filter(|entry| !claims.contains_key(&entry.effect_id))
            .take(limit)
            .map(|entry| entry.effect_id.clone())
            .collect();
        let mut claimed = Vec::new();
        for id in ids {
            let entry = outbox.get_mut(&id).expect("outbox entry");
            entry.attempts += 1;
            claims.insert(
                id.clone(),
                MemoryClaim {
                    claim_id: claim_id.to_owned(),
                    deadline_unix_ms: lease_deadline(lease),
                },
            );
            claimed.push(entry.clone());
        }
        claimed
    }
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

    fn record_claimed(
        &self,
        run_id: &RunId,
        effect: &EffectRecord,
        claim_id: &str,
    ) -> Result<(), JournalError> {
        let claims = self.claims.lock().expect("journal lock poisoned");
        match claims.get(&effect.id) {
            Some(claim) if claim.claim_id == claim_id => {}
            _ => return Err(JournalError::claim_lost(&effect.id, claim_id)),
        }
        drop(claims);
        self.record(run_id, effect)
    }

    fn renew_claim(
        &self,
        effect_id: &EffectId,
        claim_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let mut claims = self.claims.lock().expect("journal lock poisoned");
        match claims.get_mut(effect_id) {
            Some(claim) if claim.claim_id == claim_id && claim.deadline_unix_ms > now => {
                claim.deadline_unix_ms = lease_deadline(lease);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn claim_due_effects(
        &self,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(None, limit, claim_id, DEFAULT_CLAIM_LEASE))
    }

    fn claim_due_effects_for_run(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(Some(run_id), limit, claim_id, DEFAULT_CLAIM_LEASE))
    }

    fn claim_due_effects_with_lease(
        &self,
        limit: usize,
        claim_id: &str,
        lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(None, limit, claim_id, lease))
    }

    fn claim_due_effects_for_run_with_lease(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
        lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(Some(run_id), limit, claim_id, lease))
    }

    fn complete_outbox(&self, effect_id: &EffectId, claim_id: &str) -> Result<(), JournalError> {
        let mut claims = self.claims.lock().expect("journal lock poisoned");
        let owns = match claims.get(effect_id) {
            Some(claim) => claim.claim_id == claim_id,
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
            Some(claim) => claim.claim_id == claim_id,
            None => claim_id.is_empty(),
        };
        if owns {
            claims.remove(effect_id);
        }
        Ok(())
    }

    fn claim_run(
        &self,
        run_id: &RunId,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let mut leases = self.run_leases.lock().expect("journal lock poisoned");
        match leases.get(run_id) {
            Some(current) if current.is_live(now) && current.owner_id != owner_id => Ok(false),
            _ => {
                leases.insert(
                    run_id.clone(),
                    LeaseRecord {
                        owner_id: owner_id.to_owned(),
                        deadline_unix_ms: lease_deadline(lease),
                    },
                );
                Ok(true)
            }
        }
    }

    fn renew_run(
        &self,
        run_id: &RunId,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let mut leases = self.run_leases.lock().expect("journal lock poisoned");
        match leases.get_mut(run_id) {
            Some(current) if current.owner_id == owner_id && current.is_live(now) => {
                current.deadline_unix_ms = lease_deadline(lease);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn release_run(&self, run_id: &RunId, owner_id: &str) -> Result<(), JournalError> {
        let mut leases = self.run_leases.lock().expect("journal lock poisoned");
        if leases
            .get(run_id)
            .is_some_and(|lease| lease.owner_id == owner_id)
        {
            leases.remove(run_id);
        }
        Ok(())
    }

    fn run_lease(&self, run_id: &RunId) -> Result<Option<LeaseRecord>, JournalError> {
        Ok(self
            .run_leases
            .lock()
            .expect("journal lock poisoned")
            .get(run_id)
            .cloned())
    }

    fn claim_service(
        &self,
        service: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let mut leases = self.service_leases.lock().expect("journal lock poisoned");
        match leases.get(service) {
            Some(current) if current.is_live(now) && current.owner_id != owner_id => Ok(false),
            _ => {
                leases.insert(
                    service.to_owned(),
                    LeaseRecord {
                        owner_id: owner_id.to_owned(),
                        deadline_unix_ms: lease_deadline(lease),
                    },
                );
                Ok(true)
            }
        }
    }

    fn renew_service(
        &self,
        service: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let mut leases = self.service_leases.lock().expect("journal lock poisoned");
        match leases.get_mut(service) {
            Some(current) if current.owner_id == owner_id && current.is_live(now) => {
                current.deadline_unix_ms = lease_deadline(lease);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn release_service(&self, service: &str, owner_id: &str) -> Result<(), JournalError> {
        let mut leases = self.service_leases.lock().expect("journal lock poisoned");
        if leases
            .get(service)
            .is_some_and(|lease| lease.owner_id == owner_id)
        {
            leases.remove(service);
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

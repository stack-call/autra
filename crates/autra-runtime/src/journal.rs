//! In-process journal. One lock protects publication, ownership and commits;
//! this adapter makes no recovery promise after process exit.
use autra_core::{
    AcceptedEvent, AgentState, EffectId, EffectRecord, EffectStatus, EngineStateDelta, RunId,
};
pub use autra_execution::journal::{DEFAULT_CLAIM_LEASE, EffectJournal, JournalError, OutboxEntry};
use autra_execution::{
    ApprovalRecord, ApprovalStore, ApprovalStoreError, CommitAuthority, DurableTransitionStore,
    EffectResult, LeaseRecord, ResultIdentity, now_unix_ms,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[derive(Clone, Debug)]
struct MemoryClaim {
    claim_id: String,
    deadline_unix_ms: u64,
}
#[derive(Default)]
struct MemoryState {
    records: BTreeMap<RunId, Vec<EffectRecord>>,
    runs: BTreeMap<RunId, AgentState>,
    events: BTreeMap<RunId, Vec<AcceptedEvent>>,
    outbox: BTreeMap<EffectId, OutboxEntry>,
    outbox_by_run: BTreeMap<RunId, BTreeSet<EffectId>>,
    claims: BTreeMap<EffectId, MemoryClaim>,
    run_leases: BTreeMap<RunId, LeaseRecord>,
    service_leases: BTreeMap<String, LeaseRecord>,
    approvals: BTreeMap<EffectId, ApprovalRecord>,
    cancels: BTreeMap<RunId, String>,
    results: BTreeMap<RunId, Vec<(EffectResult, bool)>>,
}
#[derive(Clone, Default)]
pub struct MemoryJournal {
    state: Arc<Mutex<MemoryState>>,
}
fn deadline(lease: Duration) -> u64 {
    now_unix_ms().saturating_add(lease.as_millis() as u64)
}
impl MemoryState {
    fn record(&mut self, run: &RunId, effect: &EffectRecord) {
        let records = self.records.entry(run.clone()).or_default();
        if let Some(old) = records.iter_mut().find(|r| r.id == effect.id) {
            *old = effect.clone();
        } else {
            records.push(effect.clone());
        }
    }
    fn enqueue(&mut self, run: &RunId, effect: &EffectRecord) {
        if effect.status == EffectStatus::Authorized && !effect.intent_json.is_empty() {
            let entry = self
                .outbox
                .entry(effect.id.clone())
                .or_insert_with(|| OutboxEntry {
                    effect_id: effect.id.clone(),
                    run_id: run.clone(),
                    intent_json: effect.intent_json.clone(),
                    attempts: effect.claim_generation(),
                });
            self.outbox_by_run
                .entry(entry.run_id.clone())
                .or_default()
                .insert(effect.id.clone());
        }
    }
    fn retire_outbox(&mut self, id: &EffectId) {
        if let Some(entry) = self.outbox.remove(id) {
            if let Some(ids) = self.outbox_by_run.get_mut(&entry.run_id) {
                ids.remove(id);
                if ids.is_empty() {
                    self.outbox_by_run.remove(&entry.run_id);
                }
            }
        }
    }
    fn outbox_entries<'a>(
        &'a self,
        run: Option<&'a RunId>,
    ) -> impl Iterator<Item = &'a OutboxEntry> {
        let global = run
            .is_none()
            .then(|| self.outbox.values())
            .into_iter()
            .flatten();
        let local = run
            .and_then(|run| self.outbox_by_run.get(run))
            .into_iter()
            .flatten()
            .filter_map(|id| self.outbox.get(id));
        global.chain(local)
    }
    fn check_claim(&self, run: &RunId, id: &EffectId, claim: &str) -> Result<(), JournalError> {
        if self
            .claims
            .get(id)
            .is_some_and(|c| c.claim_id == claim && c.deadline_unix_ms > now_unix_ms())
            && self.outbox.get(id).is_some_and(|e| &e.run_id == run)
        {
            Ok(())
        } else {
            Err(JournalError::claim_lost(id, claim))
        }
    }
    fn check_owner(&self, run: &RunId, owner: &str) -> Result<(), JournalError> {
        if self
            .run_leases
            .get(run)
            .is_some_and(|l| l.owner_id == owner && l.is_live(now_unix_ms()))
        {
            Ok(())
        } else {
            Err(JournalError::new("coordinator run lease was lost"))
        }
    }
    fn check_version(
        &self,
        run: &RunId,
        expected: u64,
        state: &AgentState,
    ) -> Result<(), JournalError> {
        let current = self.runs.get(run).map_or(0, |s| s.state_version);
        if current != expected {
            return Err(JournalError::new(format!(
                "state version conflict for run `{run}`: expected {expected}, found {current}"
            )));
        }
        if &state.run_id != run || state.state_version != expected + 1 {
            return Err(JournalError::new("invalid next state identity or version"));
        }
        Ok(())
    }
    fn pending_source(&self, run: &RunId, source: &ResultIdentity) -> Result<(), JournalError> {
        if self
            .results
            .get(run)
            .is_some_and(|rs| rs.iter().any(|(r, c)| &r.identity == source && !c))
        {
            Ok(())
        } else {
            Err(JournalError::new("result is missing or already consumed"))
        }
    }
    fn consume(&mut self, run: &RunId, source: &ResultIdentity) {
        let (_, c) = self
            .results
            .get_mut(run)
            .unwrap()
            .iter_mut()
            .find(|(r, _)| &r.identity == source)
            .unwrap();
        *c = true;
    }
    fn retire_results(&mut self, run: &RunId, id: &EffectId) {
        if let Some(rs) = self.results.get_mut(run) {
            for (r, c) in rs {
                if &r.identity.effect_id == id {
                    *c = true;
                }
            }
        }
    }
    fn resolve(
        &mut self,
        run: &RunId,
        effect: &EffectRecord,
        owner: Option<&str>,
    ) -> Result<(), JournalError> {
        if !matches!(
            effect.status,
            EffectStatus::Authorized | EffectStatus::Rejected
        ) {
            return Err(JournalError::new("invalid authorization state"));
        }
        if let Some(owner) = owner {
            self.check_owner(run, owner)?;
        }
        if self
            .claims
            .get(&effect.id)
            .is_some_and(|c| c.deadline_unix_ms > now_unix_ms())
        {
            return Err(JournalError::new("cannot authorize a claimed effect"));
        }
        self.retire_results(run, &effect.id);
        self.record(run, effect);
        if effect.status == EffectStatus::Authorized {
            self.enqueue(run, effect);
        } else {
            self.retire_outbox(&effect.id);
        }
        Ok(())
    }
    fn commit(
        &mut self,
        run: &RunId,
        expected: u64,
        event: &AcceptedEvent,
        state: &AgentState,
        effects: &[EffectRecord],
        authority: Option<&CommitAuthority>,
    ) -> Result<(), JournalError> {
        if let Some(a) = authority {
            self.check_owner(run, &a.owner_id)?;
        }
        self.check_version(run, expected, state)?;
        if self
            .events
            .get(run)
            .is_some_and(|es| es.iter().any(|e| e.seq >= event.seq))
        {
            return Err(JournalError::new(format!(
                "event seq {} was already committed or out of order",
                event.seq
            )));
        }
        let source = authority.and_then(|a| a.result.as_ref());
        if let Some(s) = source {
            self.pending_source(run, s)?;
        }
        self.runs.insert(run.clone(), state.clone());
        self.events
            .entry(run.clone())
            .or_default()
            .push(event.clone());
        for e in effects {
            self.record(run, e);
            self.enqueue(run, e);
        }
        if let Some(s) = source {
            self.consume(run, s);
        }
        Ok(())
    }
}
impl MemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }
    fn claim_with_lease(
        &self,
        run: Option<&RunId>,
        limit: usize,
        claim: &str,
        lease: Duration,
    ) -> Vec<OutboxEntry> {
        let mut state = self.state.lock().expect("journal lock poisoned");
        state
            .claims
            .retain(|_, c| c.deadline_unix_ms > now_unix_ms());
        let ids: Vec<_> = state
            .outbox_entries(run)
            .filter(|e| !state.claims.contains_key(&e.effect_id))
            .take(limit)
            .map(|e| e.effect_id.clone())
            .collect();
        let mut entries = Vec::new();
        for id in ids {
            let e = state.outbox.get_mut(&id).unwrap();
            e.attempts += 1;
            entries.push(e.clone());
            state.claims.insert(
                id,
                MemoryClaim {
                    claim_id: claim.into(),
                    deadline_unix_ms: deadline(lease),
                },
            );
        }
        entries
    }
}
impl EffectJournal for MemoryJournal {
    fn record(&self, run: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .record(run, effect);
        Ok(())
    }
    fn load(&self, run: &RunId) -> Result<Vec<EffectRecord>, JournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .records
            .get(run)
            .cloned()
            .unwrap_or_default())
    }
    fn save_run(&self, state: &AgentState) -> Result<(), JournalError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .runs
            .insert(state.run_id.clone(), state.clone());
        Ok(())
    }
    fn load_run(&self, run: &RunId) -> Result<Option<AgentState>, JournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .runs
            .get(run)
            .cloned())
    }
    fn list_run_ids(&self) -> Result<Vec<RunId>, JournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .runs
            .keys()
            .cloned()
            .collect())
    }
    fn load_events(&self, run: &RunId) -> Result<Vec<AcceptedEvent>, JournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .events
            .get(run)
            .cloned()
            .unwrap_or_default())
    }
    fn list_run_ids_page(
        &self,
        after: Option<&RunId>,
        limit: usize,
    ) -> Result<Vec<RunId>, JournalError> {
        use std::ops::Bound::{Excluded, Unbounded};

        let state = self.state.lock().expect("journal lock poisoned");
        let lower = after.map_or(Unbounded, Excluded);
        Ok(state
            .runs
            .range((lower, Unbounded))
            .take(limit)
            .map(|(run_id, _)| run_id.clone())
            .collect())
    }

    fn commit_event(
        &self,
        run: &RunId,
        event: &AcceptedEvent,
        next: &AgentState,
        effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        let mut state = self.state.lock().expect("journal lock poisoned");
        state.runs.insert(run.clone(), next.clone());
        for e in effects {
            state.record(run, e);
        }
        let es = state.events.entry(run.clone()).or_default();
        if !es.iter().any(|e| e.seq == event.seq) {
            es.push(event.clone());
        }
        Ok(())
    }
    fn enqueue_effect(&self, run: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .enqueue(run, effect);
        Ok(())
    }
    fn authorize_effect(&self, run: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        if effect.status != EffectStatus::Authorized {
            return Err(JournalError::new("effect is not authorized"));
        }
        self.state
            .lock()
            .expect("journal lock poisoned")
            .resolve(run, effect, None)
    }
    fn record_claimed(
        &self,
        run: &RunId,
        effect: &EffectRecord,
        claim: &str,
    ) -> Result<(), JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        s.check_claim(run, &effect.id, claim)?;
        s.record(run, effect);
        Ok(())
    }
    fn supports_result_inbox(&self) -> bool {
        true
    }
    fn record_result_claimed(
        &self,
        run: &RunId,
        effect: &EffectRecord,
        claim: &str,
        result: &EffectResult,
    ) -> Result<(), JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        if result.identity.effect_id != effect.id {
            return Err(JournalError::new("result effect identity mismatch"));
        }
        if let Some((old, _)) = s
            .results
            .get(run)
            .and_then(|rs| rs.iter().find(|(r, _)| r.identity == result.identity))
        {
            return if old == result {
                Ok(())
            } else {
                Err(JournalError::new("conflicting result identity"))
            };
        }
        s.check_claim(run, &effect.id, claim)?;
        s.record(run, effect);
        s.results
            .entry(run.clone())
            .or_default()
            .push((result.clone(), false));
        s.retire_outbox(&effect.id);
        s.claims.remove(&effect.id);
        Ok(())
    }
    fn pending_results(
        &self,
        run: &RunId,
        limit: usize,
    ) -> Result<Vec<EffectResult>, JournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .results
            .get(run)
            .into_iter()
            .flatten()
            .filter(|(_, c)| !c)
            .take(limit)
            .map(|(r, _)| r.clone())
            .collect())
    }
    fn peek_due_effects(
        &self,
        run: Option<&RunId>,
        limit: usize,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let s = self.state.lock().expect("journal lock poisoned");
        Ok(s.outbox_entries(run)
            .filter(|e| {
                !s.claims
                    .get(&e.effect_id)
                    .is_some_and(|c| c.deadline_unix_ms > now_unix_ms())
            })
            .take(limit)
            .cloned()
            .collect())
    }
    fn claim_effect(
        &self,
        id: &EffectId,
        claim: &str,
        lease: Duration,
    ) -> Result<Option<OutboxEntry>, JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        if s.claims
            .get(id)
            .is_some_and(|c| c.deadline_unix_ms > now_unix_ms())
        {
            return Ok(None);
        }
        let Some(e) = s.outbox.get_mut(id) else {
            return Ok(None);
        };
        e.attempts += 1;
        let e = e.clone();
        s.claims.insert(
            id.clone(),
            MemoryClaim {
                claim_id: claim.into(),
                deadline_unix_ms: deadline(lease),
            },
        );
        Ok(Some(e))
    }
    fn renew_claim(
        &self,
        id: &EffectId,
        claim: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        if let Some(c) = s
            .claims
            .get_mut(id)
            .filter(|c| c.claim_id == claim && c.deadline_unix_ms > now_unix_ms())
        {
            c.deadline_unix_ms = deadline(lease);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn claim_due_effects(
        &self,
        limit: usize,
        claim: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(None, limit, claim, DEFAULT_CLAIM_LEASE))
    }
    fn claim_due_effects_for_run(
        &self,
        run: &RunId,
        limit: usize,
        claim: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(Some(run), limit, claim, DEFAULT_CLAIM_LEASE))
    }
    fn claim_due_effects_with_lease(
        &self,
        limit: usize,
        claim: &str,
        lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(None, limit, claim, lease))
    }
    fn claim_due_effects_for_run_with_lease(
        &self,
        run: &RunId,
        limit: usize,
        claim: &str,
        lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        Ok(self.claim_with_lease(Some(run), limit, claim, lease))
    }
    fn complete_outbox(&self, id: &EffectId, claim: &str) -> Result<(), JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        if s.claims
            .get(id)
            .map_or(claim.is_empty(), |c| c.claim_id == claim)
        {
            s.claims.remove(id);
            s.retire_outbox(id);
        }
        Ok(())
    }
    fn release_outbox(&self, id: &EffectId, claim: &str) -> Result<(), JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        if s.claims
            .get(id)
            .map_or(claim.is_empty(), |c| c.claim_id == claim)
        {
            s.claims.remove(id);
        }
        Ok(())
    }
    fn claim_run(&self, run: &RunId, owner: &str, lease: Duration) -> Result<bool, JournalError> {
        Ok(claim_lease(
            &mut self.state.lock().expect("journal lock poisoned").run_leases,
            run.clone(),
            owner,
            lease,
        ))
    }
    fn renew_run(&self, run: &RunId, owner: &str, lease: Duration) -> Result<bool, JournalError> {
        Ok(renew_lease(
            &mut self.state.lock().expect("journal lock poisoned").run_leases,
            run,
            owner,
            lease,
        ))
    }
    fn release_run(&self, run: &RunId, owner: &str) -> Result<(), JournalError> {
        release_lease(
            &mut self.state.lock().expect("journal lock poisoned").run_leases,
            run,
            owner,
        );
        Ok(())
    }
    fn run_lease(&self, run: &RunId) -> Result<Option<LeaseRecord>, JournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .run_leases
            .get(run)
            .cloned())
    }
    fn claim_service(&self, key: &str, owner: &str, lease: Duration) -> Result<bool, JournalError> {
        Ok(claim_lease(
            &mut self
                .state
                .lock()
                .expect("journal lock poisoned")
                .service_leases,
            key.into(),
            owner,
            lease,
        ))
    }
    fn renew_service(&self, key: &str, owner: &str, lease: Duration) -> Result<bool, JournalError> {
        Ok(renew_lease(
            &mut self
                .state
                .lock()
                .expect("journal lock poisoned")
                .service_leases,
            &key.to_owned(),
            owner,
            lease,
        ))
    }
    fn release_service(&self, key: &str, owner: &str) -> Result<(), JournalError> {
        release_lease(
            &mut self
                .state
                .lock()
                .expect("journal lock poisoned")
                .service_leases,
            &key.to_owned(),
            owner,
        );
        Ok(())
    }
    fn request_cancel(&self, run: &RunId, reason: &str) -> Result<(), JournalError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .cancels
            .insert(run.clone(), reason.into());
        Ok(())
    }
    fn cancel_request(&self, run: &RunId) -> Result<Option<String>, JournalError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .cancels
            .get(run)
            .cloned())
    }
    fn clear_cancel(&self, run: &RunId) -> Result<(), JournalError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .cancels
            .remove(run);
        Ok(())
    }
}
fn claim_lease<K: Ord>(
    leases: &mut BTreeMap<K, LeaseRecord>,
    key: K,
    owner: &str,
    lease: Duration,
) -> bool {
    if leases
        .get(&key)
        .is_some_and(|l| l.is_live(now_unix_ms()) && l.owner_id != owner)
    {
        return false;
    }
    leases.insert(
        key,
        LeaseRecord {
            owner_id: owner.into(),
            deadline_unix_ms: deadline(lease),
        },
    );
    true
}
fn renew_lease<K: Ord>(
    leases: &mut BTreeMap<K, LeaseRecord>,
    key: &K,
    owner: &str,
    lease: Duration,
) -> bool {
    if let Some(l) = leases
        .get_mut(key)
        .filter(|l| l.owner_id == owner && l.is_live(now_unix_ms()))
    {
        l.deadline_unix_ms = deadline(lease);
        true
    } else {
        false
    }
}
fn release_lease<K: Ord>(leases: &mut BTreeMap<K, LeaseRecord>, key: &K, owner: &str) {
    if leases.get(key).is_some_and(|l| l.owner_id == owner) {
        leases.remove(key);
    }
}
impl ApprovalStore for MemoryJournal {
    fn save_approval(&self, r: &ApprovalRecord) -> Result<(), ApprovalStoreError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .approvals
            .insert(r.effect_id.clone(), r.clone());
        Ok(())
    }
    fn load_approval(&self, id: &EffectId) -> Result<Option<ApprovalRecord>, ApprovalStoreError> {
        Ok(self
            .state
            .lock()
            .expect("journal lock poisoned")
            .approvals
            .get(id)
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
    fn resolve_effect_owned(
        &self,
        run: &RunId,
        effect: &EffectRecord,
        owner: &str,
    ) -> Result<(), JournalError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .resolve(run, effect, Some(owner))
    }
    fn commit_transition(
        &self,
        run: &RunId,
        expected: u64,
        event: &AcceptedEvent,
        state: &AgentState,
        effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        self.state
            .lock()
            .expect("journal lock poisoned")
            .commit(run, expected, event, state, effects, None)
    }
    fn commit_owned_transition(
        &self,
        run: &RunId,
        expected: u64,
        event: &AcceptedEvent,
        state: &AgentState,
        effects: &[EffectRecord],
        _delta: Option<&EngineStateDelta>,
        authority: &CommitAuthority,
    ) -> Result<(), JournalError> {
        self.state.lock().expect("journal lock poisoned").commit(
            run,
            expected,
            event,
            state,
            effects,
            Some(authority),
        )
    }
    fn commit_terminal(
        &self,
        run: &RunId,
        expected: u64,
        next: &AgentState,
    ) -> Result<(), JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        s.check_version(run, expected, next)?;
        s.runs.insert(run.clone(), next.clone());
        Ok(())
    }
    fn commit_owned_terminal(
        &self,
        run: &RunId,
        expected: u64,
        next: &AgentState,
        a: &CommitAuthority,
    ) -> Result<(), JournalError> {
        let mut s = self.state.lock().expect("journal lock poisoned");
        s.check_owner(run, &a.owner_id)?;
        s.check_version(run, expected, next)?;
        if let Some(r) = &a.result {
            s.pending_source(run, r)?;
        }
        s.runs.insert(run.clone(), next.clone());
        if let Some(r) = &a.result {
            s.consume(run, r);
        }
        Ok(())
    }
}

#[cfg(test)]
mod outbox_index_tests {
    use super::*;
    use autra_core::{EffectIntent, EffectKind, ModelCallSpec};
    fn effect(id: &str) -> EffectRecord {
        let mut effect = EffectRecord::new(
            EffectId::new(id),
            EffectKind::ModelCall,
            None,
            None,
            "digest".into(),
        )
        .with_intent_json(
            serde_json::to_string(&EffectIntent::CallModel(ModelCallSpec {
                system: None,
                messages: vec![],
                tools: vec![],
            }))
            .unwrap(),
        );
        effect.status = EffectStatus::Authorized;
        effect
    }
    #[test]
    fn scoped_index_preserves_order_and_is_retired_atomically() {
        let journal = MemoryJournal::new();
        let a = RunId::fresh();
        let b = RunId::fresh();
        let first = effect("a");
        let middle = effect("b");
        let mut last = effect("c");
        journal.authorize_effect(&a, &first).unwrap();
        journal.authorize_effect(&b, &middle).unwrap();
        journal.authorize_effect(&a, &last).unwrap();
        let ids = |run: Option<&RunId>| {
            journal
                .peek_due_effects(run, 10)
                .unwrap()
                .into_iter()
                .map(|e| e.effect_id.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(Some(&a)), ["a", "c"]);
        assert_eq!(ids(None), ["a", "b", "c"]);
        journal
            .claim_effect(&first.id, "worker", Duration::from_secs(60))
            .unwrap()
            .unwrap();
        assert_eq!(ids(Some(&a)), ["c"]);
        journal.release_outbox(&first.id, "worker").unwrap();
        assert_eq!(ids(Some(&a)), ["a", "c"]);
        journal
            .claim_effect(&first.id, "worker-2", Duration::from_secs(60))
            .unwrap()
            .unwrap();
        journal.complete_outbox(&first.id, "worker-2").unwrap();
        assert_eq!(ids(Some(&a)), ["c"]);
        journal
            .claim_run(&a, "coordinator", Duration::from_secs(60))
            .unwrap();
        last.status = EffectStatus::Rejected;
        journal
            .resolve_effect_owned(&a, &last, "coordinator")
            .unwrap();
        assert!(ids(Some(&a)).is_empty());
        assert_eq!(ids(None), ["b"]);
        assert!(!journal.state.lock().unwrap().outbox_by_run.contains_key(&a));
        last.status = EffectStatus::Authorized;
        journal
            .resolve_effect_owned(&a, &last, "coordinator")
            .unwrap();
        assert_eq!(ids(Some(&a)), ["c"]);
        assert_eq!(journal.state.lock().unwrap().outbox_by_run[&a].len(), 1);
    }
}

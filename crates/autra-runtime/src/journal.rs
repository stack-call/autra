//! Effect journal boundary. A durable host persists EffectRecords here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use autra_core::{EffectRecord, RunId};
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

pub trait EffectJournal: Send + Sync {
    fn record(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError>;
    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError>;
}

/// In-process journal for tests and local runs.
#[derive(Clone, Default)]
pub struct MemoryJournal {
    records: Arc<Mutex<BTreeMap<RunId, Vec<EffectRecord>>>>,
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

    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError> {
        Ok(self
            .records
            .lock()
            .expect("journal lock poisoned")
            .get(run_id)
            .cloned()
            .unwrap_or_default())
    }
}

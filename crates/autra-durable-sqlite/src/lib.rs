//! SQLite-backed EffectJournal. This v0 persists EffectRecords only; Run
//! state, run events, the outbox and recovery are future work.

use std::path::Path;
use std::sync::{Arc, Mutex};

use autra_core::{EffectRecord, RunId};
use autra_runtime::{EffectJournal, JournalError};
use rusqlite::{Connection, params};

pub struct SqliteEffectJournal {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteEffectJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let connection = Connection::open(path)
            .map_err(|error| JournalError::new(format!("cannot open journal: {error}")))?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS effects (
                    effect_id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE INDEX IF NOT EXISTS effects_run ON effects(run_id);",
            )
            .map_err(|error| JournalError::new(format!("cannot migrate journal: {error}")))?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }
}

impl EffectJournal for SqliteEffectJournal {
    fn record(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        let payload = serde_json::to_string(effect)
            .map_err(|error| JournalError::new(format!("cannot serialize effect: {error}")))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        connection
            .execute(
                "INSERT INTO effects (effect_id, run_id, payload_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(effect_id) DO UPDATE SET
                   run_id = excluded.run_id,
                   payload_json = excluded.payload_json,
                   updated_at = CURRENT_TIMESTAMP",
                params![effect.id.as_str(), run_id.as_str(), payload],
            )
            .map_err(|error| JournalError::new(format!("cannot record effect: {error}")))?;
        Ok(())
    }

    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        let mut statement = connection
            .prepare("SELECT payload_json FROM effects WHERE run_id = ?1 ORDER BY rowid")
            .map_err(|error| JournalError::new(format!("cannot read journal: {error}")))?;
        let rows = statement
            .query_map(params![run_id.as_str()], |row| row.get::<_, String>(0))
            .map_err(|error| JournalError::new(format!("cannot read journal: {error}")))?;
        let mut effects = Vec::new();
        for row in rows {
            let payload =
                row.map_err(|error| JournalError::new(format!("cannot read row: {error}")))?;
            let effect = serde_json::from_str(&payload).map_err(|error| {
                JournalError::new(format!("cannot deserialize effect: {error}"))
            })?;
            effects.push(effect);
        }
        Ok(effects)
    }
}

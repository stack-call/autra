//! SQLite-backed EffectJournal. This v0 persists EffectRecords only; Run
//! state, run events, the outbox and recovery are future work.

use std::path::Path;
use std::sync::{Arc, Mutex};

use autra_core::{AgentState, EffectRecord, RunId};
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
                CREATE INDEX IF NOT EXISTS effects_run ON effects(run_id);
                CREATE TABLE IF NOT EXISTS runs (
                    run_id TEXT PRIMARY KEY,
                    state_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
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

    fn save_run(&self, state: &AgentState) -> Result<(), JournalError> {
        let payload = serde_json::to_string(state)
            .map_err(|error| JournalError::new(format!("cannot serialize run: {error}")))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        connection
            .execute(
                "INSERT INTO runs (run_id, state_json) VALUES (?1, ?2)
                 ON CONFLICT(run_id) DO UPDATE SET
                   state_json = excluded.state_json,
                   updated_at = CURRENT_TIMESTAMP",
                params![state.run_id.as_str(), payload],
            )
            .map_err(|error| JournalError::new(format!("cannot save run: {error}")))?;
        Ok(())
    }

    fn load_run(&self, run_id: &RunId) -> Result<Option<AgentState>, JournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        let mut statement = connection
            .prepare("SELECT state_json FROM runs WHERE run_id = ?1")
            .map_err(|error| JournalError::new(format!("cannot read run: {error}")))?;
        let mut rows = statement
            .query(params![run_id.as_str()])
            .map_err(|error| JournalError::new(format!("cannot read run: {error}")))?;
        let Some(row) = rows
            .next()
            .map_err(|error| JournalError::new(format!("cannot read run: {error}")))?
        else {
            return Ok(None);
        };
        let payload: String = row
            .get(0)
            .map_err(|error| JournalError::new(format!("cannot read run: {error}")))?;
        let state = serde_json::from_str(&payload)
            .map_err(|error| JournalError::new(format!("cannot deserialize run: {error}")))?;
        Ok(Some(state))
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

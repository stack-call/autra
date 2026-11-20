//! SQLite-backed EffectJournal. This v0 persists Run snapshots, EffectRecords,
//! accepted events and the effect outbox; resume and reconciliation are future
//! work.

use std::path::Path;
use std::sync::{Arc, Mutex};

use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, EffectStatus, RunId};
use autra_runtime::{EffectJournal, JournalError, OutboxEntry};
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
                );
                CREATE TABLE IF NOT EXISTS run_events (
                    run_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    event_json TEXT NOT NULL,
                    recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (run_id, seq)
                );
                CREATE TABLE IF NOT EXISTS effect_outbox (
                    effect_id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL,
                    intent_json TEXT NOT NULL,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    claim_id TEXT,
                    claim_deadline TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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

    fn commit_event(
        &self,
        run_id: &RunId,
        event: &AcceptedEvent,
        state: &AgentState,
        effects: &[EffectRecord],
    ) -> Result<(), JournalError> {
        let event_payload = serde_json::to_string(event)
            .map_err(|error| JournalError::new(format!("cannot serialize event: {error}")))?;
        let state_payload = serde_json::to_string(state)
            .map_err(|error| JournalError::new(format!("cannot serialize run: {error}")))?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        let transaction = connection.transaction().map_err(|error| {
            JournalError::new(format!("cannot begin journal transaction: {error}"))
        })?;
        transaction
            .execute(
                "INSERT INTO runs (run_id, state_json) VALUES (?1, ?2)
                 ON CONFLICT(run_id) DO UPDATE SET
                   state_json = excluded.state_json,
                   updated_at = CURRENT_TIMESTAMP",
                params![state.run_id.as_str(), state_payload],
            )
            .map_err(|error| JournalError::new(format!("cannot save run: {error}")))?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO run_events (run_id, seq, event_json)
                 VALUES (?1, ?2, ?3)",
                params![run_id.as_str(), event.seq as i64, event_payload],
            )
            .map_err(|error| JournalError::new(format!("cannot record event: {error}")))?;
        for effect in effects {
            let payload = serde_json::to_string(effect)
                .map_err(|error| JournalError::new(format!("cannot serialize effect: {error}")))?;
            transaction
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
        }
        transaction
            .commit()
            .map_err(|error| JournalError::new(format!("cannot commit journal: {error}")))?;
        Ok(())
    }

    fn load_events(&self, run_id: &RunId) -> Result<Vec<AcceptedEvent>, JournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        let mut statement = connection
            .prepare("SELECT event_json FROM run_events WHERE run_id = ?1 ORDER BY seq")
            .map_err(|error| JournalError::new(format!("cannot read events: {error}")))?;
        let rows = statement
            .query_map(params![run_id.as_str()], |row| row.get::<_, String>(0))
            .map_err(|error| JournalError::new(format!("cannot read events: {error}")))?;
        let mut events = Vec::new();
        for row in rows {
            let payload =
                row.map_err(|error| JournalError::new(format!("cannot read event: {error}")))?;
            let event = serde_json::from_str(&payload)
                .map_err(|error| JournalError::new(format!("cannot deserialize event: {error}")))?;
            events.push(event);
        }
        Ok(events)
    }

    fn enqueue_effect(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        if effect.status != EffectStatus::Authorized || effect.intent_json.is_empty() {
            return Ok(());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        connection
            .execute(
                "INSERT OR IGNORE INTO effect_outbox (effect_id, run_id, intent_json)
                 VALUES (?1, ?2, ?3)",
                params![effect.id.as_str(), run_id.as_str(), effect.intent_json],
            )
            .map_err(|error| JournalError::new(format!("cannot enqueue effect: {error}")))?;
        Ok(())
    }

    fn claim_due_effects(
        &self,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        let transaction = connection
            .transaction()
            .map_err(|error| JournalError::new(format!("cannot begin claim: {error}")))?;
        let mut statement = transaction
            .prepare(
                "SELECT effect_id, run_id, intent_json, attempts FROM effect_outbox
                 WHERE claim_id IS NULL OR claim_deadline < datetime('now')
                 ORDER BY created_at LIMIT ?1",
            )
            .map_err(|error| JournalError::new(format!("cannot claim effects: {error}")))?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            })
            .map_err(|error| JournalError::new(format!("cannot claim effects: {error}")))?;
        let mut entries = Vec::new();
        for row in rows {
            let (effect_id, run_id, intent_json, attempts) =
                row.map_err(|error| JournalError::new(format!("cannot claim row: {error}")))?;
            transaction
                .execute(
                    "UPDATE effect_outbox SET claim_id = ?1,
                     claim_deadline = datetime('now', '+30 seconds'),
                     attempts = attempts + 1 WHERE effect_id = ?2",
                    params![claim_id, effect_id],
                )
                .map_err(|error| JournalError::new(format!("cannot claim effect: {error}")))?;
            entries.push(OutboxEntry {
                effect_id: EffectId::new(effect_id),
                run_id: RunId::new(run_id),
                intent_json,
                attempts: attempts + 1,
            });
        }
        drop(statement);
        transaction
            .commit()
            .map_err(|error| JournalError::new(format!("cannot commit claim: {error}")))?;
        Ok(entries)
    }

    fn claim_due_effects_for_run(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        let transaction = connection
            .transaction()
            .map_err(|error| JournalError::new(format!("cannot begin claim: {error}")))?;
        let mut statement = transaction
            .prepare(
                "SELECT effect_id, run_id, intent_json, attempts FROM effect_outbox
                 WHERE (claim_id IS NULL OR claim_deadline < datetime('now'))
                   AND run_id = ?1
                 ORDER BY created_at LIMIT ?2",
            )
            .map_err(|error| JournalError::new(format!("cannot claim effects: {error}")))?;
        let rows = statement
            .query_map(params![run_id.as_str(), limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            })
            .map_err(|error| JournalError::new(format!("cannot claim effects: {error}")))?;
        let mut entries = Vec::new();
        for row in rows {
            let (effect_id, run_id, intent_json, attempts) =
                row.map_err(|error| JournalError::new(format!("cannot claim row: {error}")))?;
            transaction
                .execute(
                    "UPDATE effect_outbox SET claim_id = ?1,
                     claim_deadline = datetime('now', '+30 seconds'),
                     attempts = attempts + 1 WHERE effect_id = ?2",
                    params![claim_id, effect_id],
                )
                .map_err(|error| JournalError::new(format!("cannot claim effect: {error}")))?;
            entries.push(OutboxEntry {
                effect_id: EffectId::new(effect_id),
                run_id: RunId::new(run_id),
                intent_json,
                attempts: attempts + 1,
            });
        }
        drop(statement);
        transaction
            .commit()
            .map_err(|error| JournalError::new(format!("cannot commit claim: {error}")))?;
        Ok(entries)
    }

    fn complete_outbox(&self, effect_id: &EffectId) -> Result<(), JournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        connection
            .execute(
                "DELETE FROM effect_outbox WHERE effect_id = ?1",
                params![effect_id.as_str()],
            )
            .map_err(|error| JournalError::new(format!("cannot complete effect: {error}")))?;
        Ok(())
    }

    fn release_outbox(&self, effect_id: &EffectId) -> Result<(), JournalError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        connection
            .execute(
                "UPDATE effect_outbox SET claim_id = NULL, claim_deadline = NULL
                 WHERE effect_id = ?1",
                params![effect_id.as_str()],
            )
            .map_err(|error| JournalError::new(format!("cannot release effect: {error}")))?;
        Ok(())
    }
}

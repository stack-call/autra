//! SQLite-backed EffectJournal. This v0 persists Run snapshots, EffectRecords,
//! accepted events and the effect outbox; resume and reconciliation are future
//! work.

use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, EffectStatus, RunId};
use autra_execution::{
    ApprovalRecord, ApprovalStore, ApprovalStoreError, DEFAULT_CLAIM_LEASE, DurableTransitionStore,
    EffectJournal, JournalError, LeaseRecord, OutboxEntry, now_unix_ms,
};
mod gc;
mod health;
pub mod session;

pub use gc::GcReport;
pub use health::{JournalHealth, ServiceLeaseInfo};

pub use session::SqliteSessionStore;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

/// Version written to `PRAGMA user_version`. Future schema changes must bump
/// it and add an explicit migration step.
pub(crate) const SCHEMA_VERSION: i64 = 1;

/// Shared connection settings for the journal and session stores.
pub(crate) fn configure_connection(connection: &Connection) -> Result<(), String> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    connection
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|error| error.to_string())?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "database schema version {version} is newer than supported {SCHEMA_VERSION}"
        ));
    }
    if version < SCHEMA_VERSION {
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Bounded admission for the single writer. A full queue fails fast instead of
/// growing an unbounded set of waiting writers.
struct WriteGate {
    active: AtomicUsize,
    capacity: usize,
}

impl WriteGate {
    fn enter(&self) -> Result<GateGuard<'_>, JournalError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        if active > self.capacity {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return Err(JournalError::new(
                "journal write queue is full; retry later",
            ));
        }
        Ok(GateGuard { gate: self })
    }
}

struct GateGuard<'a> {
    gate: &'a WriteGate,
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct WriteGuard<'a> {
    connection: MutexGuard<'a, Connection>,
    _gate: GateGuard<'a>,
}

impl Deref for WriteGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for WriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

pub struct SqliteEffectJournal {
    connection: Arc<Mutex<Connection>>,
    readers: Arc<Vec<Mutex<Connection>>>,
    next_reader: AtomicUsize,
    write_gate: WriteGate,
}

impl SqliteEffectJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::open_with_readers(path, 2)
    }

    pub fn open_with_readers(
        path: impl AsRef<Path>,
        read_connections: usize,
    ) -> Result<Self, JournalError> {
        let path = path.as_ref();
        let connection = Connection::open(path)
            .map_err(|error| JournalError::new(format!("cannot open journal: {error}")))?;
        configure_connection(&connection)
            .map_err(|error| JournalError::new(format!("cannot configure journal: {error}")))?;
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
                    state_version INTEGER NOT NULL DEFAULT 0,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS run_events (
                    run_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    event_json TEXT NOT NULL,
                    recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (run_id, seq)
                );
                CREATE TABLE IF NOT EXISTS approvals (
                    effect_id TEXT PRIMARY KEY,
                    payload_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS effect_outbox (
                    effect_id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL,
                    intent_json TEXT NOT NULL,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    claim_id TEXT,
                    claim_deadline TEXT,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS run_leases (
                    run_id TEXT PRIMARY KEY,
                    owner_id TEXT NOT NULL,
                    deadline_ms INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS service_leases (
                    service TEXT PRIMARY KEY,
                    owner_id TEXT NOT NULL,
                    deadline_ms INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS cancel_requests (
                    run_id TEXT PRIMARY KEY,
                    reason TEXT NOT NULL,
                    requested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
            )
            .map_err(|error| JournalError::new(format!("cannot migrate journal: {error}")))?;
        // Existing v0 databases predate optimistic state versions.
        let _ = connection.execute(
            "ALTER TABLE runs ADD COLUMN state_version INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let mut readers = Vec::new();
        for _ in 0..read_connections.max(1) {
            let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|error| {
                    JournalError::new(format!("cannot open journal reader: {error}"))
                })?;
            reader
                .busy_timeout(Duration::from_secs(5))
                .map_err(|error| JournalError::new(format!("cannot configure reader: {error}")))?;
            reader
                .execute_batch("PRAGMA query_only=ON;")
                .map_err(|error| JournalError::new(format!("cannot configure reader: {error}")))?;
            readers.push(Mutex::new(reader));
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            readers: Arc::new(readers),
            next_reader: AtomicUsize::new(0),
            write_gate: WriteGate {
                active: AtomicUsize::new(0),
                capacity: 256,
            },
        })
    }

    pub(crate) fn reader(&self) -> Result<MutexGuard<'_, Connection>, JournalError> {
        if self.readers.is_empty() {
            return self
                .connection
                .lock()
                .map_err(|_| JournalError::new("journal lock poisoned"));
        }
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        self.readers[index]
            .lock()
            .map_err(|_| JournalError::new("journal reader lock poisoned"))
    }

    pub(crate) fn writer(&self) -> Result<WriteGuard<'_>, JournalError> {
        let gate = self.write_gate.enter()?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| JournalError::new("journal lock poisoned"))?;
        Ok(WriteGuard {
            connection,
            _gate: gate,
        })
    }
}

impl SqliteEffectJournal {
    fn claim(
        &self,
        limit: usize,
        run_id: Option<&RunId>,
        claim_id: &str,
        lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let modifier = format!("+{} seconds", lease.as_secs());
        let mut connection = self.writer()?;
        let transaction = connection
            .transaction()
            .map_err(|error| JournalError::new(format!("cannot begin claim: {error}")))?;
        let mut statement = transaction
            .prepare(
                "SELECT effect_id, run_id, intent_json, attempts FROM effect_outbox
                 WHERE (claim_id IS NULL OR claim_deadline < datetime('now'))
                   AND (?1 IS NULL OR run_id = ?1)
                 ORDER BY created_at LIMIT ?2",
            )
            .map_err(|error| JournalError::new(format!("cannot claim effects: {error}")))?;
        let rows = statement
            .query_map(params![run_id.map(RunId::as_str), limit as i64], |row| {
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
                     claim_deadline = datetime('now', ?3),
                     attempts = attempts + 1 WHERE effect_id = ?2",
                    params![claim_id, effect_id, modifier],
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
}

impl EffectJournal for SqliteEffectJournal {
    fn record(&self, run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError> {
        let payload = serde_json::to_string(effect)
            .map_err(|error| JournalError::new(format!("cannot serialize effect: {error}")))?;
        let connection = self.writer()?;
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

    fn record_claimed(
        &self,
        run_id: &RunId,
        effect: &EffectRecord,
        claim_id: &str,
    ) -> Result<(), JournalError> {
        let payload = serde_json::to_string(effect)
            .map_err(|error| JournalError::new(format!("cannot serialize effect: {error}")))?;
        let mut connection = self.writer()?;
        let transaction = connection
            .transaction()
            .map_err(|error| JournalError::new(format!("cannot begin claim write: {error}")))?;
        let owned: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM effect_outbox WHERE effect_id = ?1 AND claim_id = ?2",
                params![effect.id.as_str(), claim_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| JournalError::new(format!("cannot check claim: {error}")))?;
        if owned.is_none() {
            return Err(JournalError::claim_lost(&effect.id, claim_id));
        }
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
        transaction
            .commit()
            .map_err(|error| JournalError::new(format!("cannot commit claim write: {error}")))?;
        Ok(())
    }

    fn renew_claim(
        &self,
        effect_id: &EffectId,
        claim_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let modifier = format!("+{} seconds", lease.as_secs().max(1));
        let connection = self.writer()?;
        let changed = connection
            .execute(
                "UPDATE effect_outbox
                 SET claim_deadline = datetime('now', ?3)
                 WHERE effect_id = ?1 AND claim_id = ?2
                   AND claim_deadline >= datetime('now')",
                params![effect_id.as_str(), claim_id, modifier],
            )
            .map_err(|error| JournalError::new(format!("cannot renew claim: {error}")))?;
        Ok(changed > 0)
    }

    fn save_run(&self, state: &AgentState) -> Result<(), JournalError> {
        let payload = serde_json::to_string(state)
            .map_err(|error| JournalError::new(format!("cannot serialize run: {error}")))?;
        let connection = self.writer()?;
        connection
            .execute(
                "INSERT INTO runs (run_id, state_json, state_version) VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id) DO UPDATE SET
                   state_json = excluded.state_json,
                   state_version = excluded.state_version,
                   updated_at = CURRENT_TIMESTAMP",
                params![state.run_id.as_str(), payload, state.state_version as i64],
            )
            .map_err(|error| JournalError::new(format!("cannot save run: {error}")))?;
        Ok(())
    }

    fn load_run(&self, run_id: &RunId) -> Result<Option<AgentState>, JournalError> {
        let connection = self.reader()?;
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

    fn list_run_ids(&self) -> Result<Vec<RunId>, JournalError> {
        let connection = self.reader()?;
        let mut statement = connection
            .prepare("SELECT run_id FROM runs")
            .map_err(|error| JournalError::new(format!("cannot list runs: {error}")))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| JournalError::new(format!("cannot list runs: {error}")))?;
        let mut run_ids = Vec::new();
        for row in rows {
            run_ids.push(RunId::new(row.map_err(|error| {
                JournalError::new(format!("cannot read run id: {error}"))
            })?));
        }
        Ok(run_ids)
    }

    fn load(&self, run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError> {
        let connection = self.reader()?;
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
        let mut connection = self.writer()?;
        let transaction = connection.transaction().map_err(|error| {
            JournalError::new(format!("cannot begin journal transaction: {error}"))
        })?;
        transaction
            .execute(
                "INSERT INTO runs (run_id, state_json, state_version) VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id) DO UPDATE SET
                   state_json = excluded.state_json,
                   state_version = excluded.state_version,
                   updated_at = CURRENT_TIMESTAMP",
                params![
                    state.run_id.as_str(),
                    state_payload,
                    state.state_version as i64
                ],
            )
            .map_err(|error| JournalError::new(format!("cannot save run: {error}")))?;
        transaction
            .execute(
                "INSERT INTO run_events (run_id, seq, event_json)
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
        let connection = self.reader()?;
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
        let connection = self.writer()?;
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
        self.claim(limit, None, claim_id, DEFAULT_CLAIM_LEASE)
    }

    fn claim_due_effects_for_run(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        self.claim(limit, Some(run_id), claim_id, DEFAULT_CLAIM_LEASE)
    }

    fn claim_due_effects_with_lease(
        &self,
        limit: usize,
        claim_id: &str,
        lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        self.claim(limit, None, claim_id, lease)
    }

    fn claim_due_effects_for_run_with_lease(
        &self,
        run_id: &RunId,
        limit: usize,
        claim_id: &str,
        lease: Duration,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        self.claim(limit, Some(run_id), claim_id, lease)
    }

    fn complete_outbox(&self, effect_id: &EffectId, claim_id: &str) -> Result<(), JournalError> {
        let connection = self.writer()?;
        connection
            .execute(
                "DELETE FROM effect_outbox
                 WHERE effect_id = ?1 AND (claim_id IS NULL OR claim_id = ?2)",
                params![effect_id.as_str(), claim_id],
            )
            .map_err(|error| JournalError::new(format!("cannot complete effect: {error}")))?;
        Ok(())
    }

    fn release_outbox(&self, effect_id: &EffectId, claim_id: &str) -> Result<(), JournalError> {
        let connection = self.writer()?;
        connection
            .execute(
                "UPDATE effect_outbox SET claim_id = NULL, claim_deadline = NULL
                 WHERE effect_id = ?1 AND (claim_id IS NULL OR claim_id = ?2)",
                params![effect_id.as_str(), claim_id],
            )
            .map_err(|error| JournalError::new(format!("cannot release effect: {error}")))?;
        Ok(())
    }

    fn claim_run(
        &self,
        run_id: &RunId,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let deadline = now.saturating_add(lease.as_millis() as u64);
        let connection = self.writer()?;
        let changed = connection
            .execute(
                "INSERT INTO run_leases (run_id, owner_id, deadline_ms)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id) DO UPDATE SET
                   owner_id = excluded.owner_id,
                   deadline_ms = excluded.deadline_ms
                 WHERE run_leases.deadline_ms < ?4 OR run_leases.owner_id = ?2",
                params![run_id.as_str(), owner_id, deadline as i64, now as i64],
            )
            .map_err(|error| JournalError::new(format!("cannot claim run: {error}")))?;
        Ok(changed > 0)
    }

    fn renew_run(
        &self,
        run_id: &RunId,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let deadline = now.saturating_add(lease.as_millis() as u64);
        let connection = self.writer()?;
        let changed = connection
            .execute(
                "UPDATE run_leases SET deadline_ms = ?3
                 WHERE run_id = ?1 AND owner_id = ?2 AND deadline_ms >= ?4",
                params![run_id.as_str(), owner_id, deadline as i64, now as i64],
            )
            .map_err(|error| JournalError::new(format!("cannot renew run: {error}")))?;
        Ok(changed > 0)
    }

    fn release_run(&self, run_id: &RunId, owner_id: &str) -> Result<(), JournalError> {
        let connection = self.writer()?;
        connection
            .execute(
                "DELETE FROM run_leases WHERE run_id = ?1 AND owner_id = ?2",
                params![run_id.as_str(), owner_id],
            )
            .map_err(|error| JournalError::new(format!("cannot release run: {error}")))?;
        Ok(())
    }

    fn run_lease(&self, run_id: &RunId) -> Result<Option<LeaseRecord>, JournalError> {
        let connection = self.reader()?;
        connection
            .query_row(
                "SELECT owner_id, deadline_ms FROM run_leases WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| {
                    Ok(LeaseRecord {
                        owner_id: row.get(0)?,
                        deadline_unix_ms: row.get::<_, i64>(1)? as u64,
                    })
                },
            )
            .optional()
            .map_err(|error| JournalError::new(format!("cannot read run lease: {error}")))
    }

    fn claim_service(
        &self,
        service: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let deadline = now.saturating_add(lease.as_millis() as u64);
        let connection = self.writer()?;
        let changed = connection
            .execute(
                "INSERT INTO service_leases (service, owner_id, deadline_ms)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(service) DO UPDATE SET
                   owner_id = excluded.owner_id,
                   deadline_ms = excluded.deadline_ms
                 WHERE service_leases.deadline_ms < ?4 OR service_leases.owner_id = ?2",
                params![service, owner_id, deadline as i64, now as i64],
            )
            .map_err(|error| JournalError::new(format!("cannot claim service: {error}")))?;
        Ok(changed > 0)
    }

    fn renew_service(
        &self,
        service: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, JournalError> {
        let now = now_unix_ms();
        let deadline = now.saturating_add(lease.as_millis() as u64);
        let connection = self.writer()?;
        let changed = connection
            .execute(
                "UPDATE service_leases SET deadline_ms = ?3
                 WHERE service = ?1 AND owner_id = ?2 AND deadline_ms >= ?4",
                params![service, owner_id, deadline as i64, now as i64],
            )
            .map_err(|error| JournalError::new(format!("cannot renew service: {error}")))?;
        Ok(changed > 0)
    }

    fn release_service(&self, service: &str, owner_id: &str) -> Result<(), JournalError> {
        let connection = self.writer()?;
        connection
            .execute(
                "DELETE FROM service_leases WHERE service = ?1 AND owner_id = ?2",
                params![service, owner_id],
            )
            .map_err(|error| JournalError::new(format!("cannot release service: {error}")))?;
        Ok(())
    }

    fn request_cancel(&self, run_id: &RunId, reason: &str) -> Result<(), JournalError> {
        let connection = self.writer()?;
        connection
            .execute(
                "INSERT INTO cancel_requests (run_id, reason) VALUES (?1, ?2)
                 ON CONFLICT(run_id) DO UPDATE SET
                   reason = excluded.reason,
                   requested_at = CURRENT_TIMESTAMP",
                params![run_id.as_str(), reason],
            )
            .map_err(|error| JournalError::new(format!("cannot request cancel: {error}")))?;
        Ok(())
    }

    fn cancel_request(&self, run_id: &RunId) -> Result<Option<String>, JournalError> {
        let connection = self.reader()?;
        connection
            .query_row(
                "SELECT reason FROM cancel_requests WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| JournalError::new(format!("cannot read cancel request: {error}")))
    }

    fn clear_cancel(&self, run_id: &RunId) -> Result<(), JournalError> {
        let connection = self.writer()?;
        connection
            .execute(
                "DELETE FROM cancel_requests WHERE run_id = ?1",
                params![run_id.as_str()],
            )
            .map_err(|error| JournalError::new(format!("cannot clear cancel request: {error}")))?;
        Ok(())
    }
}

impl ApprovalStore for SqliteEffectJournal {
    fn save_approval(&self, record: &ApprovalRecord) -> Result<(), ApprovalStoreError> {
        let payload = serde_json::to_string(record).map_err(|error| {
            ApprovalStoreError::new(format!("cannot serialize approval: {error}"))
        })?;
        let connection = self
            .writer()
            .map_err(|error| ApprovalStoreError::new(error.to_string()))?;
        connection
            .execute(
                "INSERT INTO approvals (effect_id, payload_json) VALUES (?1, ?2)
                 ON CONFLICT(effect_id) DO UPDATE SET
                   payload_json = excluded.payload_json,
                   updated_at = CURRENT_TIMESTAMP",
                params![record.effect_id.as_str(), payload],
            )
            .map_err(|error| ApprovalStoreError::new(format!("cannot save approval: {error}")))?;
        Ok(())
    }

    fn load_approval(
        &self,
        effect_id: &EffectId,
    ) -> Result<Option<ApprovalRecord>, ApprovalStoreError> {
        let connection = self
            .reader()
            .map_err(|error| ApprovalStoreError::new(error.to_string()))?;
        let payload: Option<String> = connection
            .query_row(
                "SELECT payload_json FROM approvals WHERE effect_id = ?1",
                params![effect_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| ApprovalStoreError::new(format!("cannot read approval: {error}")))?;
        match payload {
            Some(payload) => serde_json::from_str(&payload).map(Some).map_err(|error| {
                ApprovalStoreError::new(format!("cannot decode approval: {error}"))
            }),
            None => Ok(None),
        }
    }
}

impl DurableTransitionStore for SqliteEffectJournal {
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
        let event_payload = serde_json::to_string(event)
            .map_err(|error| JournalError::new(format!("cannot serialize event: {error}")))?;
        let state_payload = serde_json::to_string(state)
            .map_err(|error| JournalError::new(format!("cannot serialize run: {error}")))?;
        let mut connection = self.writer()?;
        let transaction = connection.transaction().map_err(|error| {
            JournalError::new(format!("cannot begin journal transaction: {error}"))
        })?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT state_version FROM runs WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| JournalError::new(format!("cannot read run version: {error}")))?;
        let current = current.unwrap_or(0) as u64;
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
        transaction
            .execute(
                "INSERT INTO runs (run_id, state_json, state_version) VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id) DO UPDATE SET
                   state_json = excluded.state_json,
                   state_version = excluded.state_version,
                   updated_at = CURRENT_TIMESTAMP",
                params![
                    state.run_id.as_str(),
                    state_payload,
                    state.state_version as i64
                ],
            )
            .map_err(|error| JournalError::new(format!("cannot save run: {error}")))?;
        transaction
            .execute(
                "INSERT INTO run_events (run_id, seq, event_json) VALUES (?1, ?2, ?3)",
                params![run_id.as_str(), event.seq as i64, event_payload],
            )
            .map_err(|error| JournalError::new(format!("cannot record event: {error}")))?;
        for effect in new_effects {
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
            if effect.status == EffectStatus::Authorized && !effect.intent_json.is_empty() {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO effect_outbox (effect_id, run_id, intent_json)
                         VALUES (?1, ?2, ?3)",
                        params![effect.id.as_str(), run_id.as_str(), effect.intent_json],
                    )
                    .map_err(|error| {
                        JournalError::new(format!("cannot enqueue effect: {error}"))
                    })?;
            }
        }
        transaction
            .commit()
            .map_err(|error| JournalError::new(format!("cannot commit journal: {error}")))?;
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
        let payload = serde_json::to_string(state)
            .map_err(|error| JournalError::new(format!("cannot serialize run: {error}")))?;
        let mut connection = self.writer()?;
        let transaction = connection.transaction().map_err(|error| {
            JournalError::new(format!("cannot begin terminal transaction: {error}"))
        })?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT state_version FROM runs WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| JournalError::new(format!("cannot read run version: {error}")))?;
        match current {
            Some(version) if version as u64 == expected_version => {
                transaction
                    .execute(
                        "UPDATE runs SET state_json = ?1, state_version = ?2,
                         updated_at = CURRENT_TIMESTAMP
                         WHERE run_id = ?3 AND state_version = ?4",
                        params![
                            payload,
                            state.state_version as i64,
                            run_id.as_str(),
                            expected_version as i64
                        ],
                    )
                    .map_err(|error| {
                        JournalError::new(format!("cannot commit terminal state: {error}"))
                    })?;
            }
            None if expected_version == 0 => {
                transaction
                    .execute(
                        "INSERT INTO runs (run_id, state_json, state_version)
                         VALUES (?1, ?2, ?3)",
                        params![run_id.as_str(), payload, state.state_version as i64],
                    )
                    .map_err(|error| {
                        JournalError::new(format!("cannot insert terminal state: {error}"))
                    })?;
            }
            Some(version) => {
                return Err(JournalError::new(format!(
                    "state version conflict for run `{run_id}`: expected {expected_version}, found {version}"
                )));
            }
            None => {
                return Err(JournalError::new(format!(
                    "state version conflict for run `{run_id}`: expected {expected_version}, found none"
                )));
            }
        }
        transaction
            .commit()
            .map_err(|error| JournalError::new(format!("cannot commit terminal state: {error}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_write_gate_rejects_a_full_queue() {
        let gate = WriteGate {
            active: AtomicUsize::new(0),
            capacity: 1,
        };
        let _first = gate.enter().unwrap();

        let second = gate.enter();

        assert!(second.is_err());
        assert_eq!(gate.active.load(Ordering::SeqCst), 1);
    }
}

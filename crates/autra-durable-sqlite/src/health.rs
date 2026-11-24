//! Read-only journal health for `autractl doctor` and operational checks.

use rusqlite::params;

use crate::{JournalError, SqliteEffectJournal};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceLeaseInfo {
    pub service: String,
    pub owner_id: String,
    pub expired: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalHealth {
    pub schema_version: i64,
    pub runs: usize,
    pub live_run_leases: usize,
    pub expired_run_leases: usize,
    pub effects: usize,
    pub pending_outbox: usize,
    pub cancel_requests: usize,
    pub service_leases: Vec<ServiceLeaseInfo>,
    pub write_queue_active: usize,
    pub write_queue_capacity: usize,
    pub readers: usize,
}

impl SqliteEffectJournal {
    pub fn health(&self, now_unix_ms: u64) -> Result<JournalHealth, JournalError> {
        let connection = self.reader()?;
        let schema_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| JournalError::new(format!("cannot read schema version: {error}")))?;
        let runs = count(&connection, "runs")?;
        let effects = count(&connection, "effects")?;
        let pending_outbox = count(&connection, "effect_outbox")?;
        let cancel_requests = count(&connection, "cancel_requests")?;
        let live_run_leases: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM run_leases WHERE deadline_ms >= ?1",
                params![now_unix_ms as i64],
                |row| row.get(0),
            )
            .map_err(|error| JournalError::new(format!("cannot count live leases: {error}")))?;
        let expired_run_leases: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM run_leases WHERE deadline_ms < ?1",
                params![now_unix_ms as i64],
                |row| row.get(0),
            )
            .map_err(|error| JournalError::new(format!("cannot count expired leases: {error}")))?;

        let mut statement = connection
            .prepare("SELECT service, owner_id, deadline_ms FROM service_leases ORDER BY service")
            .map_err(|error| JournalError::new(format!("cannot read service leases: {error}")))?;
        let rows = statement
            .query_map([], |row| {
                let deadline: i64 = row.get(2)?;
                Ok(ServiceLeaseInfo {
                    service: row.get(0)?,
                    owner_id: row.get(1)?,
                    expired: (deadline as u64) < now_unix_ms,
                })
            })
            .map_err(|error| JournalError::new(format!("cannot read service leases: {error}")))?;
        let mut service_leases = Vec::new();
        for row in rows {
            service_leases.push(
                row.map_err(|error| JournalError::new(format!("cannot read lease: {error}")))?,
            );
        }

        Ok(JournalHealth {
            schema_version,
            runs,
            live_run_leases: live_run_leases as usize,
            expired_run_leases: expired_run_leases as usize,
            effects,
            pending_outbox,
            cancel_requests,
            service_leases,
            write_queue_active: self
                .write_gate
                .active
                .load(std::sync::atomic::Ordering::Relaxed),
            write_queue_capacity: self.write_gate.capacity,
            readers: self.readers.len(),
        })
    }
}

fn count(connection: &rusqlite::Connection, table: &str) -> Result<usize, JournalError> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|error| JournalError::new(format!("cannot count {table}: {error}")))?;
    Ok(count as usize)
}

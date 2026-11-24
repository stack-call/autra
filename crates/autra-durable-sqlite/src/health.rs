//! Read-only journal health for `autractl doctor` and operational checks.

use rusqlite::params;
use serde::Serialize;

use crate::{JournalError, SqliteEffectJournal};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServiceLeaseInfo {
    pub service: String,
    pub owner_id: String,
    pub expired: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct JournalHealth {
    pub schema_version: i64,
    pub runs: usize,
    pub live_run_leases: usize,
    pub expired_run_leases: usize,
    pub effects: usize,
    pub effects_committed: usize,
    pub effects_failed: usize,
    pub effects_unknown: usize,
    pub runs_completed: usize,
    pub runs_failed: usize,
    pub runs_cancelled: usize,
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
        let effects_committed = count_where(
            &connection,
            "effects",
            "json_extract(payload_json, '$.status') = 'Committed'",
        )?;
        let effects_failed = count_where(
            &connection,
            "effects",
            "json_extract(payload_json, '$.status') = 'Failed'",
        )?;
        let effects_unknown = count_where(
            &connection,
            "effects",
            "json_extract(payload_json, '$.status') = 'OutcomeUnknown'",
        )?;
        let runs_completed = count_where(
            &connection,
            "runs",
            "json_extract(state_json, '$.phase') = 'Completed'",
        )?;
        let runs_failed = count_where(
            &connection,
            "runs",
            "json_extract(state_json, '$.phase') = 'Failed'",
        )?;
        let runs_cancelled = count_where(
            &connection,
            "runs",
            "json_extract(state_json, '$.phase') = 'Cancelled'",
        )?;
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
            effects_committed,
            effects_failed,
            effects_unknown,
            runs_completed,
            runs_failed,
            runs_cancelled,
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

fn count_where(
    connection: &rusqlite::Connection,
    table: &str,
    predicate: &str,
) -> Result<usize, JournalError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {predicate}");
    let count: i64 = connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|error| JournalError::new(format!("cannot count {table}: {error}")))?;
    Ok(count as usize)
}

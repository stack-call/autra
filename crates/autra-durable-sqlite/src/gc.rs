//! Journal retention. Only terminal Runs older than the cutoff are removed;
//! session history, approvals outside those Runs and service leases are kept.

use std::time::Duration;

use autra_core::{AgentPhase, AgentState};
use rusqlite::{OptionalExtension, params};

use crate::{JournalError, SqliteEffectJournal};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    pub runs: usize,
    pub events: usize,
    pub effects: usize,
    pub outbox: usize,
    pub approvals: usize,
    pub leases: usize,
    pub cancels: usize,
    pub engine_snapshots: usize,
    pub engine_patches: usize,
    pub dry_run: bool,
}

impl SqliteEffectJournal {
    /// Remove terminal Runs whose last update is older than `older_than`.
    /// A live Run lease is never removed, even if the timestamp is old.
    pub fn gc(&self, older_than: Duration, dry_run: bool) -> Result<GcReport, JournalError> {
        let modifier = format!("-{} seconds", older_than.as_secs());
        let mut connection = self.writer()?;
        let transaction = connection
            .transaction()
            .map_err(|error| JournalError::new(format!("cannot begin gc: {error}")))?;

        let mut candidates = Vec::new();
        {
            let mut statement = transaction
                .prepare(
                    "SELECT run_id, state_json FROM runs
                     WHERE updated_at < datetime('now', ?1)",
                )
                .map_err(|error| JournalError::new(format!("cannot list gc runs: {error}")))?;
            let rows = statement
                .query_map(params![modifier], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| JournalError::new(format!("cannot list gc runs: {error}")))?;
            for row in rows {
                let (run_id, payload) =
                    row.map_err(|error| JournalError::new(format!("cannot read gc run: {error}")))?;
                let Ok(state) = serde_json::from_str::<AgentState>(&payload) else {
                    continue;
                };
                if !matches!(
                    state.phase,
                    AgentPhase::Completed | AgentPhase::Failed | AgentPhase::Cancelled
                ) {
                    continue;
                }
                let live: Option<i64> = transaction
                    .query_row(
                        "SELECT 1 FROM run_leases WHERE run_id = ?1 AND deadline_ms >= ?2",
                        params![run_id, autra_execution::now_unix_ms() as i64],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|error| JournalError::new(format!("cannot check lease: {error}")))?;
                if live.is_none() {
                    candidates.push(run_id);
                }
            }
        }

        let mut report = GcReport {
            dry_run,
            ..GcReport::default()
        };
        for run_id in &candidates {
            report.runs += 1;
            report.events += count(&transaction, "run_events", run_id)?;
            report.effects += count(&transaction, "effects", run_id)?;
            report.outbox += count(&transaction, "effect_outbox", run_id)?;
            report.leases += count(&transaction, "run_leases", run_id)?;
            report.cancels += count(&transaction, "cancel_requests", run_id)?;
            report.engine_snapshots += count(&transaction, "run_engine_snapshots", run_id)?;
            report.engine_patches += count(&transaction, "run_engine_patches", run_id)?;

            let effect_ids = effect_ids(&transaction, run_id)?;
            for effect_id in &effect_ids {
                let approvals: i64 = transaction
                    .query_row(
                        "SELECT COUNT(*) FROM approvals WHERE effect_id = ?1",
                        params![effect_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| {
                        JournalError::new(format!("cannot count approvals: {error}"))
                    })?;
                report.approvals += approvals as usize;
            }

            if dry_run {
                continue;
            }
            transaction
                .execute(
                    "DELETE FROM effect_outbox WHERE run_id = ?1",
                    params![run_id],
                )
                .map_err(|error| JournalError::new(format!("cannot delete outbox: {error}")))?;
            transaction
                .execute("DELETE FROM run_events WHERE run_id = ?1", params![run_id])
                .map_err(|error| JournalError::new(format!("cannot delete events: {error}")))?;
            transaction
                .execute("DELETE FROM effects WHERE run_id = ?1", params![run_id])
                .map_err(|error| JournalError::new(format!("cannot delete effects: {error}")))?;
            for effect_id in &effect_ids {
                transaction
                    .execute(
                        "DELETE FROM approvals WHERE effect_id = ?1",
                        params![effect_id],
                    )
                    .map_err(|error| {
                        JournalError::new(format!("cannot delete approval: {error}"))
                    })?;
            }
            transaction
                .execute(
                    "DELETE FROM run_engine_patches WHERE run_id = ?1",
                    params![run_id],
                )
                .map_err(|error| {
                    JournalError::new(format!("cannot delete engine patch: {error}"))
                })?;
            transaction
                .execute(
                    "DELETE FROM run_engine_snapshots WHERE run_id = ?1",
                    params![run_id],
                )
                .map_err(|error| {
                    JournalError::new(format!("cannot delete engine snapshot: {error}"))
                })?;
            transaction
                .execute("DELETE FROM run_leases WHERE run_id = ?1", params![run_id])
                .map_err(|error| JournalError::new(format!("cannot delete lease: {error}")))?;
            transaction
                .execute(
                    "DELETE FROM cancel_requests WHERE run_id = ?1",
                    params![run_id],
                )
                .map_err(|error| JournalError::new(format!("cannot delete cancel: {error}")))?;
            transaction
                .execute("DELETE FROM runs WHERE run_id = ?1", params![run_id])
                .map_err(|error| JournalError::new(format!("cannot delete run: {error}")))?;
        }

        if dry_run {
            transaction
                .rollback()
                .map_err(|error| JournalError::new(format!("cannot rollback gc: {error}")))?;
        } else {
            transaction
                .commit()
                .map_err(|error| JournalError::new(format!("cannot commit gc: {error}")))?;
        }
        Ok(report)
    }
}

fn count(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    run_id: &str,
) -> Result<usize, JournalError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE run_id = ?1");
    let value: i64 = transaction
        .query_row(&sql, params![run_id], |row| row.get(0))
        .map_err(|error| JournalError::new(format!("cannot count {table}: {error}")))?;
    Ok(value as usize)
}

fn effect_ids(
    transaction: &rusqlite::Transaction<'_>,
    run_id: &str,
) -> Result<Vec<String>, JournalError> {
    let mut statement = transaction
        .prepare("SELECT effect_id FROM effects WHERE run_id = ?1")
        .map_err(|error| JournalError::new(format!("cannot list effect ids: {error}")))?;
    let rows = statement
        .query_map(params![run_id], |row| row.get::<_, String>(0))
        .map_err(|error| JournalError::new(format!("cannot list effect ids: {error}")))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(
            row.map_err(|error| JournalError::new(format!("cannot read effect id: {error}")))?,
        );
    }
    Ok(ids)
}

//! One database-wide schema migration for both stores.
use crate::SCHEMA_VERSION;
use rusqlite::{Connection, Transaction, TransactionBehavior};

const JOURNAL_SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS effects (
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
                CREATE TABLE IF NOT EXISTS result_inbox (
                    ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
                    run_id TEXT NOT NULL, effect_id TEXT NOT NULL, attempt_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL, consumed INTEGER NOT NULL DEFAULT 0,
                    UNIQUE(run_id, effect_id, attempt_id)
                );
                CREATE INDEX IF NOT EXISTS result_inbox_pending ON result_inbox(run_id, consumed, ordinal);
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
                CREATE INDEX IF NOT EXISTS effect_outbox_run_order ON effect_outbox(run_id, created_at, effect_id);
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
                );
                CREATE TABLE IF NOT EXISTS run_engine_snapshots (
                    run_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    engine_id TEXT NOT NULL,
                    schema_revision INTEGER NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (run_id, version)
                );
                CREATE TABLE IF NOT EXISTS run_engine_patches (
                    run_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    base_version INTEGER NOT NULL,
                    patch_json TEXT NOT NULL,
                    PRIMARY KEY (run_id, version)
                );"#;
const SESSION_SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS sessions (
                    session_id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS session_turns (
                    session_id TEXT NOT NULL,
                    turn_index INTEGER NOT NULL,
                    turn_id TEXT NOT NULL,
                    run_id TEXT,
                    status_json TEXT NOT NULL,
                    input TEXT NOT NULL,
                    answer TEXT,
                    error TEXT,
                    history_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (session_id, turn_index)
                );"#;

/// Serialize concurrent openers and publish the version only with all DDL.
pub(crate) fn migrate(connection: &mut Connection) -> Result<(), String> {
    migrate_with(connection, |_| Ok(()))
}

fn migrate_with(
    connection: &mut Connection,
    after_schema: impl FnOnce(&Transaction<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let version: i64 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "database schema version {version} is newer than supported {SCHEMA_VERSION}"
        ));
    }
    if version != 0 && version < 2 {
        return Err(format!(
            "journal schema version {version} is not supported; recreate the journal"
        ));
    }
    tx.execute_batch(JOURNAL_SCHEMA)
        .map_err(|error| error.to_string())?;
    // A version-zero legacy runs table may lack the optimistic version column.
    let has_state_version = {
        let mut columns = tx
            .prepare("PRAGMA table_info(runs)")
            .map_err(|error| error.to_string())?;
        let names = columns
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| error.to_string())?;
        let mut found = false;
        for name in names {
            if name.map_err(|error| error.to_string())? == "state_version" {
                found = true;
            }
        }
        found
    };
    if !has_state_version {
        tx.execute(
            "ALTER TABLE runs ADD COLUMN state_version INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    tx.execute_batch(SESSION_SCHEMA)
        .map_err(|error| error.to_string())?;
    after_schema(&tx)?;
    if version < SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|error| error.to_string())?;
    }
    tx.commit().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_migration_rolls_back_schema_and_version_then_can_retry() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE existing_data (value TEXT); INSERT INTO existing_data VALUES ('kept'); PRAGMA user_version=2;").unwrap();
        let result = migrate_with(&mut connection, |_| Err("injected failure".into()));
        assert!(result.unwrap_err().contains("injected failure"));
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('result_inbox','sessions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        let value: String = connection
            .query_row("SELECT value FROM existing_data", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "kept");
        migrate(&mut connection).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
    }
}

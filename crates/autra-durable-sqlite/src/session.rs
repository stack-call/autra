//! SQLite-backed SessionStore. Session history and execution facts stay
//! separate: this store never writes the execution journal tables.

use std::path::Path;
use std::sync::{Arc, Mutex};

use autra_core::{RunId, SessionId, TurnId};
use autra_execution::{SessionRecord, SessionStore, SessionStoreError, TurnRecord, TurnStatus};
use rusqlite::{Connection, OptionalExtension, params};

pub struct SqliteSessionStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteSessionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        let connection = Connection::open(path)
            .map_err(|error| SessionStoreError::new(format!("cannot open sessions: {error}")))?;
        super::configure_connection(&connection).map_err(|error| {
            SessionStoreError::new(format!("cannot configure sessions: {error}"))
        })?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
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
                );",
            )
            .map_err(|error| SessionStoreError::new(format!("cannot migrate sessions: {error}")))?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SessionStoreError> {
        self.connection
            .lock()
            .map_err(|_| SessionStoreError::new("session store lock poisoned"))
    }
}

impl SessionStore for SqliteSessionStore {
    fn create_session(&self, session_id: &SessionId) -> Result<(), SessionStoreError> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT OR IGNORE INTO sessions (session_id) VALUES (?1)",
                params![session_id.as_str()],
            )
            .map_err(|error| SessionStoreError::new(format!("cannot create session: {error}")))?;
        Ok(())
    }

    fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        let connection = self.lock()?;
        let exists: Option<String> = connection
            .query_row(
                "SELECT session_id FROM sessions WHERE session_id = ?1",
                params![session_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| SessionStoreError::new(format!("cannot read session: {error}")))?;
        if exists.is_none() {
            return Ok(None);
        }
        let mut statement = connection
            .prepare(
                "SELECT turn_id, turn_index, run_id, status_json, input, answer, error,
                        history_json
                 FROM session_turns WHERE session_id = ?1 ORDER BY turn_index",
            )
            .map_err(|error| SessionStoreError::new(format!("cannot read turns: {error}")))?;
        let rows = statement
            .query_map(params![session_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|error| SessionStoreError::new(format!("cannot read turns: {error}")))?;
        let mut turns = Vec::new();
        for row in rows {
            let (turn_id, index, run_id, status_json, input, answer, error, history_json) =
                row.map_err(|error| SessionStoreError::new(format!("cannot read turn: {error}")))?;
            let status: TurnStatus = serde_json::from_str(&status_json).map_err(|error| {
                SessionStoreError::new(format!("cannot decode turn status: {error}"))
            })?;
            let history = serde_json::from_str(&history_json).map_err(|error| {
                SessionStoreError::new(format!("cannot decode turn history: {error}"))
            })?;
            turns.push(TurnRecord {
                turn_id: TurnId::new(turn_id),
                index,
                run_id: run_id.map(RunId::new),
                input,
                status,
                answer,
                error,
                history,
            });
        }
        Ok(Some(SessionRecord {
            session_id: session_id.clone(),
            turns,
        }))
    }

    fn save_turn(
        &self,
        session_id: &SessionId,
        turn: &TurnRecord,
    ) -> Result<(), SessionStoreError> {
        let status_json = serde_json::to_string(&turn.status)
            .map_err(|error| SessionStoreError::new(format!("cannot encode status: {error}")))?;
        let history_json = serde_json::to_string(&turn.history)
            .map_err(|error| SessionStoreError::new(format!("cannot encode history: {error}")))?;
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO session_turns
                    (session_id, turn_index, turn_id, run_id, status_json, input, answer, error,
                     history_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(session_id, turn_index) DO UPDATE SET
                   turn_id = excluded.turn_id,
                   run_id = excluded.run_id,
                   status_json = excluded.status_json,
                   input = excluded.input,
                   answer = excluded.answer,
                   error = excluded.error,
                   history_json = excluded.history_json,
                   updated_at = CURRENT_TIMESTAMP",
                params![
                    session_id.as_str(),
                    turn.index as i64,
                    turn.turn_id.as_str(),
                    turn.run_id.as_ref().map(RunId::as_str),
                    status_json,
                    turn.input,
                    turn.answer,
                    turn.error,
                    history_json,
                ],
            )
            .map_err(|error| SessionStoreError::new(format!("cannot save turn: {error}")))?;
        connection
            .execute(
                "UPDATE sessions SET updated_at = CURRENT_TIMESTAMP WHERE session_id = ?1",
                params![session_id.as_str()],
            )
            .map_err(|error| SessionStoreError::new(format!("cannot touch session: {error}")))?;
        Ok(())
    }
}

//! Session persistence contract, separate from the execution journal.

use autra_core::{HistoryItem, RunId, SessionId, TurnId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("{0}")]
    Message(String),
}

impl SessionStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    Completed,
    Cancelled,
    Failed,
}

/// One recorded turn. `history` is the accepted conversation history after the
/// turn; a failed turn keeps the previous history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub turn_id: TurnId,
    pub index: u32,
    pub run_id: Option<RunId>,
    pub input: String,
    pub status: TurnStatus,
    pub answer: Option<String>,
    pub error: Option<String>,
    pub history: Vec<HistoryItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub turns: Vec<TurnRecord>,
}

impl SessionRecord {
    /// History to seed the next turn.
    pub fn history(&self) -> Vec<HistoryItem> {
        self.turns
            .last()
            .map(|turn| turn.history.clone())
            .unwrap_or_default()
    }

    pub fn last_turn(&self) -> Option<&TurnRecord> {
        self.turns.last()
    }
}

/// Persistence for interactive sessions. This is deliberately not the
/// execution journal: a Session store records conversation and turn metadata,
/// while the journal records authoritative Run facts.
pub trait SessionStore: Send + Sync {
    fn create_session(&self, session_id: &SessionId) -> Result<(), SessionStoreError>;

    fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError>;

    /// Insert or replace one turn, keyed by session and turn index.
    fn save_turn(&self, session_id: &SessionId, turn: &TurnRecord)
    -> Result<(), SessionStoreError>;
}

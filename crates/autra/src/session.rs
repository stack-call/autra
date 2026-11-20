use std::sync::Arc;

use autra_core::{HistoryItem, SessionId, TurnId};
use autra_execution::{SessionRecord, SessionStore, SessionStoreError, TurnRecord, TurnStatus};
use autra_harness::react_history;

use crate::{Agent, RunError, RunEvent, RunOptions, RunOutcome, RunStatus};

/// An in-memory conversation with one Agent, optionally backed by a
/// `SessionStore` for turn history.
///
/// `run_turn` requires `&mut self`, enforcing the single-writer rule: turns in
/// one Session are sequential. Each turn is a fresh Run with a fresh budget and
/// Run id, seeded with the accepted history.
pub struct Session {
    agent: Agent,
    history: Vec<HistoryItem>,
    session_id: SessionId,
    turn_index: u32,
    last_turn_id: Option<TurnId>,
    store: Option<Arc<dyn SessionStore>>,
}

impl Session {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent,
            history: Vec::new(),
            session_id: SessionId::fresh(),
            turn_index: 0,
            last_turn_id: None,
            store: None,
        }
    }

    /// Open (or create) a persisted session.
    pub fn open(
        agent: Agent,
        store: Arc<dyn SessionStore>,
        session_id: SessionId,
    ) -> Result<Self, SessionStoreError> {
        store.create_session(&session_id)?;
        let record = store.load_session(&session_id)?.unwrap_or(SessionRecord {
            session_id: session_id.clone(),
            turns: Vec::new(),
        });
        let history = record.history();
        let turn_index = record.turns.len() as u32;
        let last_turn_id = record.last_turn().map(|turn| turn.turn_id.clone());
        Ok(Self {
            agent,
            history,
            session_id,
            turn_index,
            last_turn_id,
            store: Some(store),
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn turn_index(&self) -> u32 {
        self.turn_index
    }

    pub fn last_turn_id(&self) -> Option<&TurnId> {
        self.last_turn_id.as_ref()
    }

    pub fn history(&self) -> &[HistoryItem] {
        &self.history
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub async fn run_turn(&mut self, input: &str) -> Result<RunOutcome, RunError> {
        self.run_turn_with_options(input, RunOptions::default(), |_| {})
            .await
    }

    #[tracing::instrument(skip_all, fields(session_id = %self.session_id, turn = self.turn_index + 1))]
    pub async fn run_turn_with_options<F>(
        &mut self,
        input: &str,
        options: RunOptions,
        mut on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let turn_id = TurnId::fresh();
        self.turn_index += 1;
        self.last_turn_id = Some(turn_id.clone());
        let mut options = options;
        options.session_id = Some(self.session_id.clone());
        options.turn_id = Some(turn_id.clone());
        let result = self
            .agent
            .run_with_history(&self.history, input, options, |event| on_event(event))
            .await;
        match result {
            Ok(outcome) => {
                self.history = react_history(outcome.state());
                if let Some(store) = &self.store {
                    let status = match outcome.status() {
                        RunStatus::Completed => TurnStatus::Completed,
                        RunStatus::Cancelled => TurnStatus::Cancelled,
                    };
                    let turn = TurnRecord {
                        turn_id,
                        index: self.turn_index,
                        run_id: Some(outcome.state().run_id.clone()),
                        input: input.to_owned(),
                        status,
                        answer: outcome.answer().map(str::to_owned),
                        error: None,
                        history: self.history.clone(),
                    };
                    store
                        .save_turn(&self.session_id, &turn)
                        .map_err(|error| RunError::Session(error.to_string()))?;
                }
                Ok(outcome)
            }
            Err(error) => {
                if let Some(store) = &self.store {
                    let turn = TurnRecord {
                        turn_id,
                        index: self.turn_index,
                        run_id: None,
                        input: input.to_owned(),
                        status: TurnStatus::Failed,
                        answer: None,
                        error: Some(error.to_string()),
                        history: self.history.clone(),
                    };
                    store
                        .save_turn(&self.session_id, &turn)
                        .map_err(|store_error| RunError::Session(store_error.to_string()))?;
                }
                Err(error)
            }
        }
    }
}

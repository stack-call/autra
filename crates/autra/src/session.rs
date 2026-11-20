use autra_core::{HistoryItem, SessionId, TurnId};

use crate::{Agent, RunError, RunEvent, RunOptions, RunOutcome};

/// An in-memory conversation with one Agent.
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
}

impl Session {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent,
            history: Vec::new(),
            session_id: SessionId::fresh(),
            turn_index: 0,
            last_turn_id: None,
        }
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
        options.turn_id = Some(turn_id);
        let outcome = self
            .agent
            .run_with_history(&self.history, input, options, |event| on_event(event))
            .await?;
        self.history = outcome.state().history.clone();
        Ok(outcome)
    }
}

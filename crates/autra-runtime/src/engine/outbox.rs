//! Outbox draining: claim due Effects across Runs that have no live owner.

use autra_execution::RunError;

use crate::dispatcher::{DEFAULT_CLAIM_LEASE, DispatchOptions, DispatchResult, OutboxDispatcher};

use super::LocalEngine;

impl LocalEngine {
    /// Continue a Run from its last committed checkpoint. Requires a journal.
    pub async fn drain_outbox(&self, limit: usize) -> Result<Vec<DispatchResult>, RunError> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("outbox drain requires a journal".to_owned()))?;
        let run_ids = journal
            .journal()
            .list_run_ids()
            .map_err(|error| RunError::Journal(error.to_string()))?;
        if run_ids.is_empty() {
            return OutboxDispatcher::run_due(
                journal.journal(),
                limit,
                self.dispatcher.as_ref(),
                self.effect_timeout,
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()));
        }
        let mut results = Vec::new();
        for run_id in run_ids {
            if results.len() >= limit {
                break;
            }
            if journal
                .journal()
                .run_lease(&run_id)
                .map_err(|error| RunError::Journal(error.to_string()))?
                .is_some_and(|lease| lease.is_live(autra_execution::now_unix_ms()))
            {
                // A live executor owns this Run; its own loop drains it.
                continue;
            }
            let options = DispatchOptions {
                limit: limit - results.len(),
                max_parallel: 1,
                timeout: self.effect_timeout,
                cancel: None,
                lease: DEFAULT_CLAIM_LEASE,
                budget: None,
                child_execution: None,
                delta: None,
                scheduler: Some(self.scheduler.clone()),
            };
            let mut batch = OutboxDispatcher::run_once_for_run_with(
                journal.journal(),
                &run_id,
                options,
                self.dispatcher.as_ref(),
                |_| {},
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()))?;
            results.append(&mut batch);
        }
        Ok(results)
    }
}

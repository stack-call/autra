//! Recovery supervisor: claim the journal's recovery service, scan unfinished
//! Runs and reuse the normal resume path. It is not a second Run loop.

use std::sync::Arc;
use std::time::Duration;

use autra_core::{AgentStepper, CancelFlag};
use autra_execution::{DurableTransitionStore, RecoveryReport, RunError};

use super::super::LocalEngine;
use super::super::lease::next_owner_id;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_LIMIT: usize = 32;
const MIN_SERVICE_LEASE: Duration = Duration::from_secs(30);

/// Owns a service lease so only one supervisor scans a journal at a time.
pub struct RecoveryLease {
    journal: Arc<dyn DurableTransitionStore>,
    service: String,
    owner: String,
}

impl Drop for RecoveryLease {
    fn drop(&mut self) {
        let _ = self.journal.release_service(&self.service, &self.owner);
    }
}

/// Runtime-owned recovery loop. The CLI daemon is now a thin wrapper around it.
pub struct RecoverySupervisor {
    engine: LocalEngine,
    stepper: Arc<dyn AgentStepper>,
    interval: Duration,
    limit: usize,
    service: String,
    lease: Duration,
    owner: String,
}

impl RecoverySupervisor {
    pub fn new(engine: LocalEngine, stepper: Arc<dyn AgentStepper>) -> Self {
        Self {
            engine,
            stepper,
            interval: DEFAULT_INTERVAL,
            limit: DEFAULT_LIMIT,
            service: "recovery-daemon".to_owned(),
            lease: MIN_SERVICE_LEASE,
            owner: next_owner_id("supervisor"),
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval.max(Duration::from_secs(1));
        self.lease = (self.interval * 3).max(MIN_SERVICE_LEASE);
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = service.into();
        self
    }

    /// Claim the recovery service. The returned guard releases it on drop.
    pub fn claim(&self) -> Result<RecoveryLease, RunError> {
        let journal = self
            .engine
            .journal
            .clone()
            .ok_or_else(|| RunError::Protocol("recovery requires a journal".to_owned()))?;
        if !journal
            .claim_service(&self.service, &self.owner, self.lease)
            .map_err(|error| RunError::Journal(error.to_string()))?
        {
            return Err(RunError::Protocol(
                "another recovery supervisor already owns this journal".to_owned(),
            ));
        }
        Ok(RecoveryLease {
            journal,
            service: self.service.clone(),
            owner: self.owner.clone(),
        })
    }

    fn renew(&self) -> Result<(), RunError> {
        let journal = self
            .engine
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("recovery requires a journal".to_owned()))?;
        let renewed = journal
            .renew_service(&self.service, &self.owner, self.lease)
            .map_err(|error| RunError::Journal(error.to_string()))?;
        if !renewed {
            return Err(RunError::Protocol(
                "recovery service lease was lost".to_owned(),
            ));
        }
        Ok(())
    }

    /// One leased pass. Useful for `daemon --once` and tests.
    pub async fn run_once_leased(&self) -> Result<RecoveryReport, RunError> {
        let _lease = self.claim()?;
        self.run_once().await
    }

    pub async fn run_once(&self) -> Result<RecoveryReport, RunError> {
        self.engine
            .recover_once(self.stepper.clone(), self.limit)
            .await
    }

    /// Scan until the cancel flag is set, renewing the service lease between
    /// passes. A lost lease stops the loop instead of competing with another
    /// supervisor.
    pub async fn run_until<F>(&self, cancel: CancelFlag, mut on_report: F) -> Result<(), RunError>
    where
        F: FnMut(&RecoveryReport),
    {
        let _lease = self.claim()?;
        loop {
            let report = self.run_once().await?;
            on_report(&report);
            if cancel.is_cancelled() {
                return Ok(());
            }
            tokio::select! {
                _ = tokio::time::sleep(self.interval) => {}
                _ = wait_for_cancel(&cancel) => return Ok(()),
            }
            self.renew()?;
        }
    }
}

async fn wait_for_cancel(cancel: &CancelFlag) {
    while !cancel.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

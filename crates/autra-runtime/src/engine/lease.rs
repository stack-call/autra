//! Run ownership. A Run may have at most one live executor across local
//! processes; the lease is renewed while the executor is alive.

use std::sync::Arc;
use std::time::Duration;

use autra_core::{CancelFlag, RunId};
use autra_execution::{DurableTransitionStore, RunError};

/// Lease length for a live Run. A crashed owner is reclaimable after this.
pub(super) const RUN_LEASE: Duration = Duration::from_secs(60);

/// Holds the Run lease and cancels the Run if the lease is lost.
pub(super) struct RunLeaseGuard {
    journal: Option<Arc<dyn DurableTransitionStore>>,
    run_id: RunId,
    owner_id: String,
    renewal: Option<tokio::task::JoinHandle<()>>,
}

impl RunLeaseGuard {
    pub(super) fn commit_owner(&self) -> Option<String> {
        self.journal
            .as_ref()
            .filter(|j| j.supports_result_inbox())
            .map(|_| self.owner_id.clone())
    }
    pub(super) fn acquire(
        journal: Option<Arc<dyn DurableTransitionStore>>,
        run_id: &RunId,
        cancel: CancelFlag,
    ) -> Result<Self, RunError> {
        let Some(journal) = journal else {
            return Ok(Self {
                journal: None,
                run_id: run_id.clone(),
                owner_id: String::new(),
                renewal: None,
            });
        };
        let owner_id = next_owner_id("run");
        let claimed = journal
            .claim_run(run_id, &owner_id, RUN_LEASE)
            .map_err(|error| RunError::Journal(error.to_string()))?;
        if !claimed {
            return Err(RunError::Protocol(format!(
                "run `{run_id}` is owned by another executor"
            )));
        }
        let renewal_journal = journal.clone();
        let renewal_run = run_id.clone();
        let renewal_owner = owner_id.clone();
        let renewal = tokio::spawn(async move {
            let period = (RUN_LEASE / 3).max(Duration::from_millis(100));
            let poll = Duration::from_secs(1).min(period);
            let mut last_renew = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(poll).await;
                if matches!(renewal_journal.cancel_request(&renewal_run), Ok(Some(_))) {
                    cancel.cancel();
                }
                let elapsed = last_renew.elapsed();
                if elapsed < period {
                    continue;
                }
                let renewed = renewal_journal.renew_run(&renewal_run, &renewal_owner, RUN_LEASE);
                tracing::debug!(
                    target: "autra::runtime_latency",
                    renewal_start_lag_us = (elapsed - period).as_micros() as u64,
                    renewal_end_lag_us = last_renew.elapsed().saturating_sub(period).as_micros() as u64,
                );
                match renewed {
                    Ok(true) => last_renew = tokio::time::Instant::now(),
                    _ => {
                        cancel.cancel();
                        break;
                    }
                }
            }
        });
        Ok(Self {
            journal: Some(journal),
            run_id: run_id.clone(),
            owner_id,
            renewal: Some(renewal),
        })
    }
}

impl Drop for RunLeaseGuard {
    fn drop(&mut self) {
        if let Some(renewal) = self.renewal.take() {
            renewal.abort();
        }
        if let Some(journal) = &self.journal {
            let _ = journal.release_run(&self.run_id, &self.owner_id);
        }
    }
}

pub(crate) fn next_owner_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{}-{nanos}-{counter}", std::process::id())
}

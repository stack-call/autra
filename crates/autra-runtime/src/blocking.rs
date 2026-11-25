//! Offload synchronous journal work to Tokio's blocking pool.
//!
//! SQLite commits are synchronous. Running them inline occupies a Tokio worker
//! for the duration of the write, which delays unrelated Runs. The helpers here
//! move the work to `spawn_blocking` and hand the owned state back.

use autra_execution::{JournalError, RunError};

/// Variant that returns an owned value alongside the journal result, so a
/// caller can move a large state into the blocking task and get it back
/// without cloning.
pub(crate) async fn run_journal_with<F, T>(task: F) -> Result<T, RunError>
where
    F: FnOnce() -> (T, Result<(), JournalError>) + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(task).await {
        Ok((value, result)) => {
            result.map_err(|error| RunError::Journal(error.to_string()))?;
            Ok(value)
        }
        Err(error) => Err(RunError::Journal(format!("journal task failed: {error}"))),
    }
}

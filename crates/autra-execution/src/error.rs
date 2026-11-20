//! Shared error type for execution contracts.

use thiserror::Error;

#[derive(Debug, Error)]
#[error("{0}")]
pub struct ContractError(String);

impl ContractError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

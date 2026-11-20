//! Sandbox backend contract. No backend is implemented yet.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

pub const SANDBOX_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxCapabilities {
    pub protocol_version: u32,
    pub persistent_workspace: bool,
    pub snapshot: bool,
    pub command_execution: bool,
    pub file_api: bool,
    pub network_policy: bool,
    pub credential_injection: bool,
}

impl Default for SandboxCapabilities {
    fn default() -> Self {
        Self {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            persistent_workspace: false,
            snapshot: false,
            command_execution: false,
            file_api: false,
            network_policy: false,
            credential_injection: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SandboxSpec {
    pub workspace_root: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxHandle {
    pub id: String,
}

#[derive(Clone, Debug)]
pub struct SandboxCommand {
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxStatus {
    pub running: bool,
    pub workspace_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("unsupported sandbox capability: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
}

/// Provision, execute, inspect and release an isolated environment.
///
/// `release` is not a promise that the workspace can be recovered later.
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    fn capabilities(&self) -> SandboxCapabilities;

    fn protocol_version(&self) -> u32 {
        SANDBOX_PROTOCOL_VERSION
    }

    async fn provision(&self, spec: SandboxSpec) -> Result<SandboxHandle, SandboxError>;

    async fn execute(
        &self,
        handle: &SandboxHandle,
        command: SandboxCommand,
    ) -> Result<SandboxOutput, SandboxError>;

    async fn inspect(&self, handle: &SandboxHandle) -> Result<SandboxStatus, SandboxError>;

    async fn release(&self, handle: SandboxHandle) -> Result<(), SandboxError>;
}

//! Resource contracts, independent of Run acceptance and journaling.
use async_trait::async_trait;
use autra_core::CancelFlag;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub const ENVIRONMENT_PROTOCOL_VERSION: u32 = 1;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentCapabilities {
    pub file_api: bool,
    pub command_execution: bool,
    pub persistent_workspace: bool,
    pub snapshot: bool,
}
#[derive(Debug, Error)]
pub enum EnvironmentError {
    #[error("unsupported environment capability: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    OutcomeUnknown(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    pub kind: FileKind,
    pub len: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: FileKind,
}
/// A bounded read, with at most one extra byte for truncation detection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRead {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}
#[derive(Clone, Debug)]
pub enum EnvironmentCommand {
    Program { program: String, args: Vec<String> },
    Shell(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}
fn unsupported(capability: &str) -> EnvironmentError {
    EnvironmentError::Unsupported(capability.to_owned())
}

/// One concrete resource location. Paths are environment-relative and must
/// reject escapes. No operation may fall back to another resource location.
#[async_trait]
pub trait ExecutionEnvironment: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EnvironmentCapabilities;
    fn protocol_version(&self) -> u32 {
        ENVIRONMENT_PROTOCOL_VERSION
    }
    fn read_preview(&self, _path: &str, _max_bytes: usize) -> Option<String> {
        None
    }
    async fn metadata(&self, _path: &str) -> Result<FileMetadata, EnvironmentError> {
        Err(unsupported("file_api"))
    }
    async fn read_file(
        &self,
        _path: &str,
        _max_bytes: usize,
    ) -> Result<FileRead, EnvironmentError> {
        Err(unsupported("file_api"))
    }
    async fn list_directory(&self, _path: &str) -> Result<Vec<DirectoryEntry>, EnvironmentError> {
        Err(unsupported("file_api"))
    }
    async fn write_file(&self, _path: &str, _content: &str) -> Result<(), EnvironmentError> {
        Err(unsupported("file_api"))
    }
    /// Must be atomic relative to other writers, never separate read/write calls.
    async fn write_file_if_unchanged(
        &self,
        _path: &str,
        _expected: &str,
        _content: &str,
    ) -> Result<(), EnvironmentError> {
        Err(unsupported("atomic_compare_write"))
    }
    async fn execute(
        &self,
        _command: EnvironmentCommand,
        _timeout: Duration,
        _cancel: Option<CancelFlag>,
    ) -> Result<CommandOutput, EnvironmentError> {
        Err(unsupported("command_execution"))
    }
}
/// Resolves a stable resource identity. An unavailable persistent workspace
/// must not be silently replaced with a fresh one.
#[async_trait]
pub trait EnvironmentProvider: Send + Sync {
    async fn acquire(&self, id: &str) -> Result<Arc<dyn ExecutionEnvironment>, EnvironmentError>;
    async fn release(&self, id: &str) -> Result<(), EnvironmentError>;
}

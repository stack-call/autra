//! Local resource adapter, retaining the LocalWorkspace compatibility surface.
use crate::workspace::LocalWorkspace;
use async_trait::async_trait;
use autra_core::CancelFlag;
use autra_execution::{
    CommandOutput, DirectoryEntry, EnvironmentCapabilities, EnvironmentCommand, EnvironmentError,
    EnvironmentProvider, ExecutionEnvironment, FileKind, FileMetadata, FileRead, ToolError,
};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
fn local_error(error: ToolError) -> EnvironmentError {
    if error.outcome_unknown {
        EnvironmentError::OutcomeUnknown(error.message)
    } else {
        EnvironmentError::Failed(error.message)
    }
}
#[cfg(feature = "process")]
async fn blocking<F, T>(task: F) -> Result<T, EnvironmentError>
where
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|e| EnvironmentError::Failed(format!("blocking task failed: {e}")))?
        .map_err(local_error)
}
#[cfg(not(feature = "process"))]
async fn blocking<F, T>(task: F) -> Result<T, EnvironmentError>
where
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    task().map_err(local_error)
}
#[derive(Clone, Debug)]
pub struct LocalEnvironment {
    id: String,
    workspace: LocalWorkspace,
}
impl LocalEnvironment {
    pub fn new(workspace: LocalWorkspace) -> Self {
        Self {
            id: format!("local:{}", workspace.root().display()),
            workspace,
        }
    }
    fn read(&self, path: &str, max_bytes: usize) -> Result<FileRead, ToolError> {
        let resolved = self.workspace.resolve(path)?;
        if !std::fs::metadata(&resolved)
            .map_err(|e| ToolError::new(e.to_string()))?
            .is_file()
        {
            return Err(ToolError::new(format!("`{path}` is not a file")));
        }
        let file = std::fs::File::open(resolved)
            .map_err(|e| ToolError::new(format!("cannot read `{path}`: {e}")))?;
        let mut bytes = Vec::new();
        file.take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| ToolError::new(e.to_string()))?;
        Ok(FileRead {
            truncated: bytes.len() > max_bytes,
            bytes,
        })
    }
}
#[async_trait]
impl ExecutionEnvironment for LocalEnvironment {
    fn id(&self) -> &str {
        &self.id
    }
    fn capabilities(&self) -> EnvironmentCapabilities {
        EnvironmentCapabilities {
            file_api: true,
            command_execution: cfg!(feature = "process"),
            persistent_workspace: true,
            snapshot: false,
        }
    }
    fn read_preview(&self, path: &str, max_bytes: usize) -> Option<String> {
        let r = self.read(path, max_bytes).ok()?;
        if r.truncated {
            None
        } else {
            String::from_utf8(r.bytes).ok()
        }
    }
    async fn metadata(&self, path: &str) -> Result<FileMetadata, EnvironmentError> {
        let workspace = self.workspace.clone();
        let path = path.to_owned();
        blocking(move || {
            let p = workspace.resolve(&path)?;
            let m = std::fs::metadata(p).map_err(|e| ToolError::new(e.to_string()))?;
            Ok(FileMetadata {
                kind: if m.is_dir() {
                    FileKind::Directory
                } else {
                    FileKind::File
                },
                len: m.len(),
            })
        })
        .await
    }
    async fn read_file(&self, path: &str, max_bytes: usize) -> Result<FileRead, EnvironmentError> {
        let env = self.clone();
        let path = path.to_owned();
        blocking(move || env.read(&path, max_bytes)).await
    }
    async fn list_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>, EnvironmentError> {
        let workspace = self.workspace.clone();
        let path = path.to_owned();
        blocking(move || {
            let p = workspace.resolve(&path)?;
            let entries = std::fs::read_dir(p).map_err(|e| ToolError::new(e.to_string()))?;
            let mut result = Vec::new();
            for e in entries {
                let e = e.map_err(|e| ToolError::new(e.to_string()))?;
                let k = e.file_type().map_err(|e| ToolError::new(e.to_string()))?;
                result.push(DirectoryEntry {
                    name: e.file_name().to_string_lossy().into_owned(),
                    kind: if k.is_symlink() {
                        FileKind::Symlink
                    } else if k.is_dir() {
                        FileKind::Directory
                    } else {
                        FileKind::File
                    },
                });
            }
            Ok(result)
        })
        .await
    }
    async fn write_file(&self, path: &str, content: &str) -> Result<(), EnvironmentError> {
        let workspace = self.workspace.clone();
        let (path, content) = (path.to_owned(), content.to_owned());
        blocking(move || workspace.write_atomic(&path, &content)).await
    }
    async fn write_file_if_unchanged(
        &self,
        path: &str,
        expected: &str,
        content: &str,
    ) -> Result<(), EnvironmentError> {
        let workspace = self.workspace.clone();
        let (path, expected, content) = (path.to_owned(), expected.to_owned(), content.to_owned());
        blocking(move || workspace.write_atomic_if_unchanged(&path, &expected, &content)).await
    }
    async fn execute(
        &self,
        command: EnvironmentCommand,
        timeout: Duration,
        cancel: Option<CancelFlag>,
    ) -> Result<CommandOutput, EnvironmentError> {
        #[cfg(feature = "process")]
        {
            let (program, args) = match command {
                EnvironmentCommand::Program { program, args } => (program, args),
                EnvironmentCommand::Shell(command) => {
                    let (program, flag) = if cfg!(windows) {
                        ("cmd", "/C")
                    } else {
                        ("sh", "-c")
                    };
                    (program.to_owned(), vec![flag.to_owned(), command])
                }
            };
            crate::environment_process::run_program(
                self.workspace.root(),
                &program,
                &args,
                timeout,
                cancel,
            )
            .await
            .map_err(local_error)
        }
        #[cfg(not(feature = "process"))]
        {
            let _ = (command, timeout, cancel);
            Err(EnvironmentError::Unsupported(
                "command_execution".to_owned(),
            ))
        }
    }
}
/// Owns one configured local workspace; release does not delete its files.
pub struct LocalEnvironmentProvider(pub LocalEnvironment);
#[async_trait]
impl EnvironmentProvider for LocalEnvironmentProvider {
    async fn acquire(&self, id: &str) -> Result<Arc<dyn ExecutionEnvironment>, EnvironmentError> {
        if id != self.0.id() {
            return Err(EnvironmentError::Failed(format!(
                "unknown environment `{id}`"
            )));
        }
        Ok(Arc::new(self.0.clone()))
    }
    async fn release(&self, id: &str) -> Result<(), EnvironmentError> {
        if id != self.0.id() {
            return Err(EnvironmentError::Failed(format!(
                "unknown environment `{id}`"
            )));
        }
        Ok(())
    }
}

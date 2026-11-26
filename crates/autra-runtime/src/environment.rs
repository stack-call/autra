//! Sandbox adapter; providers own resources, never execution authority.
use async_trait::async_trait;
use autra_core::CancelFlag;
pub use autra_execution::environment::*;
use autra_execution::{SandboxBackend, SandboxCommand, SandboxError, SandboxHandle, SandboxSpec};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
fn backend_error(error: SandboxError) -> EnvironmentError {
    match error {
        SandboxError::Unsupported(c) => EnvironmentError::Unsupported(c),
        SandboxError::Failed(m) => EnvironmentError::Failed(m),
    }
}
fn write_error(error: SandboxError) -> EnvironmentError {
    match error {
        SandboxError::Unsupported(c) => EnvironmentError::Unsupported(c),
        SandboxError::Failed(m) => EnvironmentError::OutcomeUnknown(m),
    }
}
pub struct SandboxEnvironment {
    id: String,
    backend: Arc<dyn SandboxBackend>,
    handle: SandboxHandle,
}
#[async_trait]
impl ExecutionEnvironment for SandboxEnvironment {
    fn id(&self) -> &str {
        &self.id
    }
    fn capabilities(&self) -> EnvironmentCapabilities {
        let c = self.backend.capabilities();
        EnvironmentCapabilities {
            file_api: c.file_api,
            command_execution: c.command_execution,
            persistent_workspace: c.persistent_workspace,
            snapshot: c.snapshot,
        }
    }
    async fn metadata(&self, path: &str) -> Result<FileMetadata, EnvironmentError> {
        self.backend
            .metadata(&self.handle, path)
            .await
            .map_err(backend_error)
    }
    async fn read_file(&self, path: &str, max_bytes: usize) -> Result<FileRead, EnvironmentError> {
        self.backend
            .read_file(&self.handle, path, max_bytes)
            .await
            .map_err(backend_error)
    }
    async fn list_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>, EnvironmentError> {
        self.backend
            .list_directory(&self.handle, path)
            .await
            .map_err(backend_error)
    }
    async fn write_file(&self, path: &str, content: &str) -> Result<(), EnvironmentError> {
        self.backend
            .write_file(&self.handle, path, content)
            .await
            .map_err(write_error)
    }
    async fn write_file_if_unchanged(
        &self,
        path: &str,
        expected: &str,
        content: &str,
    ) -> Result<(), EnvironmentError> {
        self.backend
            .write_file_if_unchanged(&self.handle, path, expected, content)
            .await
            .map_err(write_error)
    }
    async fn execute(
        &self,
        command: EnvironmentCommand,
        timeout: Duration,
        cancel: Option<CancelFlag>,
    ) -> Result<CommandOutput, EnvironmentError> {
        if cancel.as_ref().is_some_and(CancelFlag::is_cancelled) {
            return Err(EnvironmentError::Failed("cancelled before dispatch".into()));
        }
        let (program, args) = match command {
            EnvironmentCommand::Program { program, args } => (program, args),
            EnvironmentCommand::Shell(command) => ("sh".into(), vec!["-c".into(), command]),
        };
        let call = self.backend.execute(
            &self.handle,
            SandboxCommand {
                program,
                args,
                timeout: Some(timeout),
            },
        );
        let output = crate::dispatcher::await_with_cancel(
            tokio::time::timeout(timeout, call),
            cancel.as_ref(),
        )
        .await;
        let output = match output {
            Some(Ok(output)) => output.map_err(write_error)?,
            Some(Err(_)) => {
                return Err(EnvironmentError::OutcomeUnknown(
                    "sandbox command timed out after dispatch".into(),
                ));
            }
            None => {
                return Err(EnvironmentError::OutcomeUnknown(
                    "sandbox command cancelled after dispatch".into(),
                ));
            }
        };
        Ok(CommandOutput {
            code: Some(output.exit_code),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}
pub struct SandboxEnvironmentProvider {
    backend: Arc<dyn SandboxBackend>,
    environments: Mutex<BTreeMap<String, Arc<SandboxEnvironment>>>,
}
impl SandboxEnvironmentProvider {
    pub fn new(backend: Arc<dyn SandboxBackend>) -> Self {
        Self {
            backend,
            environments: Mutex::new(BTreeMap::new()),
        }
    }
    pub async fn provision(
        &self,
        spec: SandboxSpec,
    ) -> Result<Arc<dyn ExecutionEnvironment>, EnvironmentError> {
        if self.backend.protocol_version() != autra_execution::SANDBOX_PROTOCOL_VERSION
            || self.backend.capabilities().protocol_version
                != autra_execution::SANDBOX_PROTOCOL_VERSION
        {
            return Err(EnvironmentError::Unsupported(
                "sandbox protocol version".into(),
            ));
        }
        let handle = self.backend.provision(spec).await.map_err(backend_error)?;
        let id = format!("sandbox:{}", handle.id);
        let env = Arc::new(SandboxEnvironment {
            id: id.clone(),
            backend: self.backend.clone(),
            handle,
        });
        let mut environments = self.environments.lock().expect("environment lock poisoned");
        if environments.contains_key(&id) {
            return Err(EnvironmentError::Failed(format!(
                "backend reused live environment identity `{id}`"
            )));
        }
        environments.insert(id, env.clone());
        Ok(env)
    }
}
#[async_trait]
impl EnvironmentProvider for SandboxEnvironmentProvider {
    async fn acquire(&self, id: &str) -> Result<Arc<dyn ExecutionEnvironment>, EnvironmentError> {
        self.environments
            .lock()
            .expect("environment lock poisoned")
            .get(id)
            .cloned()
            .map(|e| e as Arc<dyn ExecutionEnvironment>)
            .ok_or_else(|| EnvironmentError::Failed(format!("unknown environment `{id}`")))
    }
    async fn release(&self, id: &str) -> Result<(), EnvironmentError> {
        let env = self
            .environments
            .lock()
            .expect("environment lock poisoned")
            .get(id)
            .cloned()
            .ok_or_else(|| EnvironmentError::Failed(format!("unknown environment `{id}`")))?;
        self.backend
            .release(env.handle.clone())
            .await
            .map_err(backend_error)?;
        self.environments
            .lock()
            .expect("environment lock poisoned")
            .remove(id);
        Ok(())
    }
}

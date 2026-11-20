use std::time::Duration;

use async_trait::async_trait;
use autra_runtime::{
    ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities, LocalHost, SandboxBackend,
    SandboxCapabilities, SandboxCommand, SandboxError, SandboxHandle, SandboxOutput, SandboxSpec,
    SandboxStatus,
};

struct FakeSandbox;

#[async_trait]
impl SandboxBackend for FakeSandbox {
    fn capabilities(&self) -> SandboxCapabilities {
        SandboxCapabilities {
            persistent_workspace: true,
            command_execution: true,
            file_api: true,
            ..SandboxCapabilities::default()
        }
    }

    async fn provision(&self, _spec: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        Ok(SandboxHandle {
            id: "sandbox-1".to_owned(),
        })
    }

    async fn execute(
        &self,
        _handle: &SandboxHandle,
        _command: SandboxCommand,
    ) -> Result<SandboxOutput, SandboxError> {
        Ok(SandboxOutput {
            exit_code: 0,
            stdout: "ok".to_owned(),
            stderr: String::new(),
        })
    }

    async fn inspect(&self, _handle: &SandboxHandle) -> Result<SandboxStatus, SandboxError> {
        Ok(SandboxStatus {
            running: true,
            workspace_id: None,
        })
    }

    async fn release(&self, _handle: SandboxHandle) -> Result<(), SandboxError> {
        Ok(())
    }
}

#[tokio::test]
async fn sandbox_lifecycle_contract() {
    let sandbox = FakeSandbox;
    assert!(sandbox.capabilities().persistent_workspace);
    assert_eq!(sandbox.protocol_version(), 1);

    let handle = sandbox.provision(SandboxSpec::default()).await.unwrap();
    let output = sandbox
        .execute(
            &handle,
            SandboxCommand {
                program: "echo".to_owned(),
                args: vec!["ok".to_owned()],
                timeout: Some(Duration::from_secs(1)),
            },
        )
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(sandbox.inspect(&handle).await.unwrap().running);
    sandbox.release(handle).await.unwrap();
}

#[test]
fn local_host_declares_no_durable_guarantees() {
    let capabilities = LocalHost.capabilities();
    assert_eq!(capabilities, HostCapabilities::default());
    assert_eq!(capabilities.protocol_version, HOST_PROTOCOL_VERSION);
    assert!(!capabilities.crash_recovery);
    assert!(!capabilities.effect_reconciliation);
}

use async_trait::async_trait;
use autra_core::{EnvironmentAffinity, ExecutionRequirements, RunId};
use autra_execution::{
    EnvironmentCapabilities, EnvironmentCommand, EnvironmentError, EnvironmentProvider,
    ExecutionEnvironment, SandboxBackend, SandboxCapabilities, SandboxCommand, SandboxError,
    SandboxHandle, SandboxOutput, SandboxSpec, SandboxStatus,
};
use autra_runtime::{GlobalScheduler, ResourceKind, SandboxEnvironmentProvider, SchedulerLimits};
use std::sync::Arc;
use std::time::Duration;
struct Environment(&'static str);
#[async_trait]
impl ExecutionEnvironment for Environment {
    fn id(&self) -> &str {
        self.0
    }
    fn capabilities(&self) -> EnvironmentCapabilities {
        EnvironmentCapabilities {
            file_api: true,
            ..Default::default()
        }
    }
}
fn required(id: &str) -> ExecutionRequirements {
    ExecutionRequirements {
        environment: Some(EnvironmentAffinity::Required(id.into())),
        file_api: true,
        command_execution: false,
    }
}
#[tokio::test]
async fn affinity_and_environment_quotas_are_independent_of_global_capacity() {
    let scheduler = GlobalScheduler::new(SchedulerLimits::new(4, 4, 4));
    scheduler
        .register_environment(Arc::new(Environment("one")), 1)
        .unwrap();
    scheduler
        .register_environment(Arc::new(Environment("two")), 1)
        .unwrap();
    let run = RunId::fresh();
    let one = required("one");
    let two = required("two");
    let held = scheduler
        .acquire_effect(ResourceKind::Tool, &run, None, &one, None)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            scheduler.acquire_effect(ResourceKind::Tool, &run, None, &one, None)
        )
        .await
        .is_err()
    );
    let other = scheduler
        .acquire_effect(ResourceKind::Tool, &run, None, &two, None)
        .await
        .unwrap();
    assert_eq!(other.environment.as_ref().unwrap().id(), "two");
    assert!(
        scheduler
            .acquire_effect(ResourceKind::Tool, &run, None, &required("missing"), None)
            .await
            .is_err()
    );
    drop(other);
    drop(held);
    let preferred = ExecutionRequirements {
        environment: Some(EnvironmentAffinity::Preferred("missing".into())),
        ..two
    };
    assert!(
        scheduler
            .acquire_effect(ResourceKind::Tool, &run, None, &preferred, None)
            .await
            .unwrap()
            .environment
            .is_some()
    );
}
#[tokio::test]
async fn dropping_a_waiter_after_handoff_returns_its_permit() {
    let scheduler = GlobalScheduler::new(SchedulerLimits::new(1, 1, 1));
    let run = RunId::fresh();
    let held = scheduler
        .acquire(ResourceKind::Tool, &run, None)
        .await
        .unwrap();
    let mut waiter = Box::pin(scheduler.acquire(ResourceKind::Tool, &run, None));
    tokio::select! {_= &mut waiter=>panic!("pool must be occupied"),_=tokio::time::sleep(Duration::from_millis(5))=>{}}
    drop(held);
    drop(waiter);
    tokio::time::timeout(
        Duration::from_millis(50),
        scheduler.acquire(ResourceKind::Tool, &run, None),
    )
    .await
    .unwrap()
    .unwrap();
}
struct Sandbox;
#[async_trait]
impl SandboxBackend for Sandbox {
    fn capabilities(&self) -> SandboxCapabilities {
        SandboxCapabilities {
            command_execution: true,
            ..Default::default()
        }
    }
    async fn provision(&self, _: SandboxSpec) -> Result<SandboxHandle, SandboxError> {
        Ok(SandboxHandle { id: "one".into() })
    }
    async fn execute(
        &self,
        _: &SandboxHandle,
        c: SandboxCommand,
    ) -> Result<SandboxOutput, SandboxError> {
        if c.program == "slow" {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        Ok(SandboxOutput {
            exit_code: 0,
            stdout: c.args.join(" "),
            stderr: String::new(),
        })
    }
    async fn inspect(&self, _: &SandboxHandle) -> Result<SandboxStatus, SandboxError> {
        Ok(SandboxStatus {
            running: true,
            workspace_id: None,
        })
    }
    async fn release(&self, _: SandboxHandle) -> Result<(), SandboxError> {
        Ok(())
    }
}
#[tokio::test]
async fn sandbox_adapter_preserves_unsupported_operations_and_unknown_timeouts() {
    let p = SandboxEnvironmentProvider::new(Arc::new(Sandbox));
    let env = p.provision(SandboxSpec::default()).await.unwrap();
    assert_eq!(p.acquire(env.id()).await.unwrap().id(), env.id());
    let o = env
        .execute(
            EnvironmentCommand::Program {
                program: "echo".into(),
                args: vec!["remote".into()],
            },
            Duration::from_secs(1),
            None,
        )
        .await
        .unwrap();
    assert_eq!(o.stdout, "remote");
    assert!(matches!(
        env.read_file("Cargo.toml", 100).await,
        Err(EnvironmentError::Unsupported(_))
    ));
    assert!(matches!(
        env.execute(
            EnvironmentCommand::Program {
                program: "slow".into(),
                args: vec![]
            },
            Duration::from_millis(5),
            None
        )
        .await
        .unwrap_err(),
        EnvironmentError::OutcomeUnknown(_)
    ));
    p.release(env.id()).await.unwrap();
    assert!(p.acquire(env.id()).await.is_err());
}

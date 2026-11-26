//! Local execution host.
//!
//! Owns the local execution coordinator and the public Agent facade. This is
//! the `LocalEphemeral` host; durability and sandbox backends are separate,
//! optional crates.

pub mod approval;
mod blocking;
pub mod child;
pub mod dispatcher;
pub mod engine;
pub mod environment;
pub mod hooks;
pub mod host;
pub mod hub;
pub mod journal;
pub mod policy;
pub mod sandbox;
pub mod scheduler;

pub use approval::{ApprovalDecision, ApprovalRequest, Approver, digest_arguments};
pub use autra_execution::{RunError, RunEvent, RunOptions, RunOutcome, RunStatus};
pub use child::{ChildHandle, ChildRunSet, ChildSpec, JoinPolicy};
pub use dispatcher::{
    DEFAULT_CLAIM_LEASE, DispatchOptions, DispatchOutcome, DispatchResult, Dispatcher,
    LocalDispatcher, OutboxDispatcher,
};
pub use engine::{
    EffectExecutor, ExecutionDependencies, LocalEngine, RecoveryLease, RecoverySupervisor,
};
pub use hooks::ObserverHook;
pub use host::{ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities, LocalHost};
pub use hub::{RunHub, RunSubscription};
pub use journal::{EffectJournal, JournalError, MemoryJournal, OutboxEntry};
pub use policy::RulePolicy;
pub use sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxBackend, SandboxCapabilities, SandboxCommand, SandboxError,
    SandboxHandle, SandboxOutput, SandboxSpec, SandboxStatus,
};
pub use scheduler::{GlobalScheduler, ResourceKind, SchedulerLimits};

pub use environment::{SandboxEnvironment, SandboxEnvironmentProvider};

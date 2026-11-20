//! Local execution host.
//!
//! Owns the local execution coordinator and the public Agent facade. This is
//! the `LocalEphemeral` host; durability and sandbox backends are separate,
//! optional crates.

pub mod approval;
pub mod dispatcher;
pub mod engine;
pub mod hooks;
pub mod host;
pub mod journal;
pub mod sandbox;

pub use approval::{ApprovalDecision, ApprovalRequest, Approver, digest_arguments};
pub use autra_execution::{RunError, RunEvent, RunOptions, RunOutcome, RunStatus};
pub use dispatcher::{
    DEFAULT_CLAIM_LEASE, DispatchOptions, DispatchOutcome, DispatchResult, Dispatcher,
    LocalDispatcher, OutboxDispatcher,
};
pub use engine::LocalEngine;
pub use hooks::ObserverHook;
pub use host::{ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities, LocalHost};
pub use journal::{EffectJournal, JournalError, MemoryJournal, OutboxEntry};
pub use sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxBackend, SandboxCapabilities, SandboxCommand, SandboxError,
    SandboxHandle, SandboxOutput, SandboxSpec, SandboxStatus,
};

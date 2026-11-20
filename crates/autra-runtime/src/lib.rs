//! Local execution host.
//!
//! Owns the local execution coordinator and the public Agent facade. This is
//! the `LocalEphemeral` host; durability and sandbox backends are separate,
//! optional crates.

pub mod agent;
pub mod approval;
pub mod hooks;
pub mod host;
pub mod journal;
pub mod sandbox;
pub mod session;
pub mod subagent;

pub use agent::{
    Agent, AgentBuilder, BuildError, RunError, RunEvent, RunOptions, RunOutcome, RunStatus,
};
pub use approval::{ApprovalDecision, ApprovalRequest, Approver, digest_arguments};
pub use hooks::ObserverHook;
pub use host::{ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities, LocalHost};
pub use journal::{EffectJournal, JournalError, MemoryJournal};
pub use sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxBackend, SandboxCapabilities, SandboxCommand, SandboxError,
    SandboxHandle, SandboxOutput, SandboxSpec, SandboxStatus,
};
pub use session::Session;
pub use subagent::{Subagent, SubagentRegistry, SubagentTool};

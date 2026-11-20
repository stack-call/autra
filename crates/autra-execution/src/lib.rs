//! Stable execution contracts.
//!
//! These types are shared by the harness (which produces behaviour) and by
//! execution hosts and adapters (which own reliability). They depend on
//! `autra-core` only; no Tokio, database or product facade.

pub mod approval;
pub mod hooks;
pub mod host;
pub mod journal;
pub mod model;
pub mod policy;
pub mod run;
pub mod sandbox;
pub mod session;
pub mod tool;

pub use approval::{
    ApprovalDecision, ApprovalRequest, Approver, DEFAULT_APPROVAL_TTL, LOCAL_POLICY_REVISION,
    digest_arguments, digest_intent,
};
pub use hooks::ObserverHook;
pub use host::{ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities};
pub use journal::{
    DEFAULT_CLAIM_LEASE, DurableTransitionStore, EffectJournal, JournalError, OutboxEntry,
};
pub use model::{MODEL_PROTOCOL_VERSION, ModelCapabilities, ModelError, ModelProvider};
pub use policy::{PolicyChain, PolicyDecision, PolicyRequest, ToolPolicy, baseline_decision};
pub use run::{RunError, RunEvent, RunOptions, RunOutcome, RunStatus};
pub use sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxBackend, SandboxCapabilities, SandboxCommand, SandboxError,
    SandboxHandle, SandboxOutput, SandboxSpec, SandboxStatus,
};
pub use session::{SessionRecord, SessionStore, SessionStoreError, TurnRecord, TurnStatus};
pub use tool::{
    TOOL_PROTOCOL_VERSION, Tool, ToolContext, ToolContract, ToolDescriptor, ToolError, ToolInput,
    ToolOutput, ToolRegistry,
};

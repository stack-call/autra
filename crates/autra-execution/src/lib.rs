//! Stable execution contracts.
//!
//! These types are shared by the harness (which produces behaviour) and by
//! execution hosts and adapters (which own reliability). They depend on
//! `autra-core` only; no Tokio, database or product facade.

pub mod approval;
pub mod error;
pub mod hooks;
pub mod host;
pub mod journal;
pub mod model;
pub mod policy;
pub mod reconcile;
pub mod run;
pub mod sandbox;
pub mod session;
pub mod tool;

pub use approval::{
    ApprovalDecision, ApprovalRecord, ApprovalRequest, ApprovalStore, ApprovalStoreError, Approver,
    DEFAULT_APPROVAL_TTL, LOCAL_POLICY_REVISION, digest_arguments, digest_intent, now_unix_ms,
};
pub use error::ContractError;
pub use hooks::ObserverHook;
pub use host::{ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities};
pub use journal::{
    DEFAULT_CLAIM_LEASE, DurableTransitionStore, EffectJournal, JournalError, OutboxEntry,
};
pub use model::{
    MODEL_PROTOCOL_VERSION, ModelCapabilities, ModelError, ModelProvider, emit_complete_text,
};
pub use policy::{PolicyChain, PolicyDecision, PolicyRequest, ToolPolicy, baseline_decision};
pub use reconcile::{ReconciliationReport, UnresolvedEffect};
pub use run::{RunError, RunEvent, RunOptions, RunOutcome, RunSnapshot, RunStatus};
pub use sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxBackend, SandboxCapabilities, SandboxCommand, SandboxError,
    SandboxHandle, SandboxOutput, SandboxSpec, SandboxStatus,
};
pub use session::{SessionRecord, SessionStore, SessionStoreError, TurnRecord, TurnStatus};
pub use tool::{
    ChildExecutionContext, TOOL_PROTOCOL_VERSION, Tool, ToolContext, ToolContract, ToolDescriptor,
    ToolError, ToolInput, ToolOutput, ToolRegistry,
};

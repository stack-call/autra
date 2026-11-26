//! Product facade for Autra.
//!
//! Assembles harness behaviour (ReAct, context, tools, skills) with the local
//! execution engine. The facade owns the developer-facing API; it does not own
//! execution semantics or persistence.

pub mod agent;
pub mod session;
pub mod subagent;

pub use agent::{Agent, AgentBuilder, BuildError};
pub use autra_execution::{
    ApprovalDecision, ApprovalRecord, ApprovalRequest, ApprovalStore, ApprovalStoreError, Approver,
    ChildExecutionContext, DEFAULT_APPROVAL_TTL, DEFAULT_CLAIM_LEASE, DurableTransitionStore,
    EffectJournal, ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities, JournalError,
    LOCAL_POLICY_REVISION, MODEL_PROTOCOL_VERSION, ModelCapabilities, ModelError, ModelProvider,
    ObserverHook, OutboxEntry, PolicyChain, PolicyDecision, PolicyRequest, ReconciliationReport,
    RecoveryReport, RunError, RunEvent, RunOptions, RunOutcome, RunSnapshot, RunStatus,
    SANDBOX_PROTOCOL_VERSION, SandboxBackend, SandboxCapabilities, SandboxCommand, SandboxError,
    SandboxHandle, SandboxOutput, SandboxSpec, SandboxStatus, SessionRecord, SessionStore,
    SessionStoreError, TOOL_PROTOCOL_VERSION, Tool, ToolContext, ToolContract, ToolDescriptor,
    ToolError, ToolInput, ToolOutput, ToolPolicy, ToolRegistry, TurnRecord, TurnStatus,
    digest_arguments, now_unix_ms,
};
pub use autra_harness::{react_engine_state, react_history, react_pending_tool_calls};
pub use autra_runtime::{
    ChildHandle, ChildRunSet, ChildSpec, DispatchOptions, DispatchOutcome, DispatchResult,
    Dispatcher, JoinPolicy, LocalDispatcher, LocalEngine, MemoryJournal, OutboxDispatcher,
    RecoveryLease, RecoverySupervisor, RulePolicy, RunHub, RunSubscription, SchedulerLimits,
};
pub use session::Session;
pub use subagent::{Subagent, SubagentRegistry, SubagentTool};

pub use autra_execution::environment::*;
pub use autra_harness::{LocalEnvironment, LocalEnvironmentProvider};

pub use autra_runtime::{EffectExecutor, ExecutionDependencies, SandboxEnvironmentProvider};

pub use autra_execution::{CommitAuthority, EffectOutcome, EffectResult, ResultIdentity};
pub use autra_runtime::{GlobalScheduler, ResourceKind};

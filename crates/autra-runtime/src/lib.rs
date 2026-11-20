//! Local execution host.
//!
//! Owns the local execution coordinator and the public Agent facade. This is
//! the `LocalEphemeral` host; durability and sandbox backends are separate,
//! optional crates.

pub mod agent;
pub mod approval;
pub mod session;
pub mod subagent;

pub use agent::{
    Agent, AgentBuilder, BuildError, RunError, RunEvent, RunOptions, RunOutcome, RunStatus,
};
pub use approval::{ApprovalDecision, ApprovalRequest, Approver, digest_arguments};
pub use session::Session;
pub use subagent::{Subagent, SubagentRegistry, SubagentTool};

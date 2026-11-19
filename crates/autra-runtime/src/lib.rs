//! Local execution host.
//!
//! Owns the local execution coordinator and the public Agent facade. This is
//! the `LocalEphemeral` host; durability and sandbox backends are separate,
//! optional crates.

pub mod agent;

pub use agent::{
    Agent, AgentBuilder, BuildError, RunError, RunEvent, RunOptions, RunOutcome, RunStatus,
};

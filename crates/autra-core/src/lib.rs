//! Sans-I/O agent core.
//!
//! The core answers exactly one question: given the accepted events and state,
//! what is the next execution intent? It performs no I/O and must not depend on
//! Tokio, a database, a sandbox backend, or any harness/runtime crate.

pub mod effect;
pub mod event;
pub mod ids;
pub mod message;
pub mod scope;
pub mod state;
pub mod stepper;

pub use effect::{
    AttemptStatus, EffectAttempt, EffectClass, EffectIntent, EffectKind, EffectRecord,
    EffectStatus, ModelCallSpec, ToolCallSpec,
};
pub use event::{AcceptedEvent, EventKind};
pub use ids::{EffectId, RunId, SessionId, TurnId};
pub use message::{HistoryItem, ModelOutput, ToolCallRequest, ToolOutcome, ToolSpec};
pub use scope::{BudgetSnapshot, CancelFlag, ExecutionScope};
pub use state::{AgentPhase, AgentState, EngineState};
pub use stepper::{AgentStepper, Disposition, STEPPER_PROTOCOL_VERSION, StepError, Transition};

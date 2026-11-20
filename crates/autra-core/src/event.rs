use serde::{Deserialize, Serialize};

use crate::message::{ModelOutput, ToolOutcome};

/// An event accepted by the coordinator, in authoritative order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedEvent {
    pub seq: u64,
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    RunStarted {
        input: String,
    },
    ModelResult(ModelOutput),
    ToolResult(ToolOutcome),
    CancelRequested,
    /// Continue a checkpointed Run from its current state.
    ResumeRequested,
}

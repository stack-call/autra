use serde::{Deserialize, Serialize};

use crate::message::{HistoryItem, ToolCallRequest, ToolSpec};

/// An execution intent produced by the stepper and executed by the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectIntent {
    CallModel(ModelCallSpec),
    CallTool(ToolCallSpec),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCallSpec {
    pub system: Option<String>,
    pub messages: Vec<HistoryItem>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSpec {
    pub call: ToolCallRequest,
}

/// A hint for execution and review. It is not an external guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectClass {
    ReadOnly,
    Idempotent,
    Reconcileable,
    NonIdempotent,
    Unknown,
}

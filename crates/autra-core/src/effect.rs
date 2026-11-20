use serde::{Deserialize, Serialize};

use crate::ids::EffectId;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    ModelCall,
    ToolCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectStatus {
    Recorded,
    AwaitingApproval,
    Authorized,
    Dispatched,
    Committed,
    Failed,
    Rejected,
    OutcomeUnknown,
    CancelledBeforeDispatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptStatus {
    Succeeded,
    Failed,
    OutcomeUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectAttempt {
    pub attempt: u32,
    pub status: AttemptStatus,
    pub result: Option<String>,
    pub error: Option<String>,
}

/// Host-owned record of one accepted logical effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectRecord {
    pub id: EffectId,
    pub kind: EffectKind,
    pub tool_name: Option<String>,
    pub call_id: Option<String>,
    pub status: EffectStatus,
    pub intent_digest: String,
    #[serde(default)]
    pub intent_json: String,
    pub attempts: Vec<EffectAttempt>,
    pub result: Option<String>,
    pub error: Option<String>,
    /// Reconciliation notes, kept separate from attempt errors.
    #[serde(default)]
    pub reconciliations: Vec<String>,
}

impl EffectRecord {
    pub fn new(
        id: EffectId,
        kind: EffectKind,
        tool_name: Option<String>,
        call_id: Option<String>,
        intent_digest: String,
    ) -> Self {
        Self {
            id,
            kind,
            tool_name,
            call_id,
            status: EffectStatus::Recorded,
            intent_digest,
            intent_json: String::new(),
            attempts: Vec::new(),
            result: None,
            error: None,
            reconciliations: Vec::new(),
        }
    }

    pub fn with_intent_json(mut self, intent_json: impl Into<String>) -> Self {
        self.intent_json = intent_json.into();
        self
    }

    pub fn set_status(&mut self, status: EffectStatus) {
        self.status = status;
    }

    pub fn note_reconciliation(&mut self, reason: impl Into<String>) {
        self.reconciliations.push(reason.into());
    }

    pub fn record_attempt(
        &mut self,
        status: AttemptStatus,
        result: Option<String>,
        error: Option<String>,
    ) -> u32 {
        let attempt = self.attempts.len() as u32 + 1;
        if result.is_some() {
            self.result = result.clone();
        }
        if error.is_some() {
            self.error = error.clone();
        }
        self.attempts.push(EffectAttempt {
            attempt,
            status,
            result,
            error,
        });
        attempt
    }
}

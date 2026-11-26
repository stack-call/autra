use serde::{Deserialize, Serialize};

use crate::ids::{AttemptId, EffectId};
use crate::message::{HistoryItem, ToolCallRequest, ToolOutcome, ToolSpec};

/// An execution intent produced by the stepper and executed by the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectIntent {
    CallModel(ModelCallSpec),
    CallTool(ToolCallSpec),
}

impl EffectIntent {
    pub fn tool_call(&self) -> Option<&ToolCallRequest> {
        match self {
            EffectIntent::CallTool(spec) => Some(&spec.call),
            EffectIntent::CallModel(_) => None,
        }
    }

    pub fn into_tool_call(self) -> Option<ToolCallRequest> {
        match self {
            EffectIntent::CallTool(spec) => Some(spec.call),
            EffectIntent::CallModel(_) => None,
        }
    }
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

/// Persisted resource placement, interpreted by the execution runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvironmentAffinity {
    Any,
    Preferred(String),
    Required(String),
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRequirements {
    pub environment: Option<EnvironmentAffinity>,
    pub file_api: bool,
    pub command_execution: bool,
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
    /// Stable identity for this physical attempt. Older journals may omit it.
    #[serde(default)]
    pub id: Option<AttemptId>,
    pub status: AttemptStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    /// Executor that claimed the attempt, when the host records one.
    #[serde(default)]
    pub worker_id: Option<String>,
    /// Monotonic claim generation used to fence stale executors.
    #[serde(default)]
    pub fencing_token: Option<u64>,
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
    #[serde(default)]
    pub requirements: ExecutionRequirements,
    /// This Effect delivers its result through the Journal Inbox.
    #[serde(default)]
    pub result_inbox: bool,
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
            requirements: ExecutionRequirements::default(),
            result_inbox: false,
        }
    }

    /// Keep claim generations monotonic when reconciliation recreates Outbox.
    pub fn claim_generation(&self) -> u32 {
        self.attempts
            .iter()
            .filter_map(|a| a.fencing_token)
            .max()
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32
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

    /// Apply a tool outcome using the shared status/attempt rules.
    pub fn apply_tool_outcome(&mut self, outcome: &ToolOutcome, known_tool: bool) {
        if outcome.outcome_unknown {
            self.set_status(EffectStatus::OutcomeUnknown);
            self.record_attempt(
                AttemptStatus::OutcomeUnknown,
                Some(outcome.content.clone()),
                None,
            );
        } else if !known_tool {
            self.set_status(EffectStatus::Failed);
            self.record_attempt(AttemptStatus::Failed, None, Some(outcome.content.clone()));
        } else {
            let error = if outcome.is_error {
                Some(outcome.content.clone())
            } else {
                None
            };
            self.set_status(EffectStatus::Committed);
            self.record_attempt(
                AttemptStatus::Succeeded,
                Some(outcome.content.clone()),
                error,
            );
        }
    }

    pub fn apply_model_success(&mut self, text: Option<String>) {
        self.set_status(EffectStatus::Committed);
        self.record_attempt(AttemptStatus::Succeeded, text, None);
    }

    pub fn apply_model_failure(&mut self, message: impl Into<String>) {
        self.set_status(EffectStatus::Failed);
        self.record_attempt(AttemptStatus::Failed, None, Some(message.into()));
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
            id: Some(AttemptId::fresh()),
            status,
            result,
            error,
            worker_id: None,
            fencing_token: None,
        });
        attempt
    }
}

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use async_trait::async_trait;
use autra_core::{EffectId, EffectIntent, EffectRecord, RunId, ToolCallRequest};
use sha2::{Digest, Sha256};

pub const LOCAL_POLICY_REVISION: &str = "local-v1";
pub const DEFAULT_APPROVAL_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub effect_id: EffectId,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
    pub argument_digest: String,
    pub policy_revision: String,
    /// Who is asked to approve, when the host knows.
    pub actor: Option<String>,
    /// Unix milliseconds after which this approval is no longer valid.
    pub expires_at_unix_ms: Option<u64>,
    /// Tool-provided description of the pending change, if the tool has one.
    pub preview: Option<String>,
}

impl ApprovalRequest {
    pub fn new(run_id: &RunId, effect_id: &EffectId, call: &ToolCallRequest) -> Self {
        Self {
            effect_id: effect_id.clone(),
            run_id: run_id.to_string(),
            call_id: call.call_id.clone(),
            tool_name: call.name.clone(),
            arguments: call.arguments.clone(),
            argument_digest: digest_arguments(&call.name, &call.arguments),
            policy_revision: LOCAL_POLICY_REVISION.to_owned(),
            actor: None,
            expires_at_unix_ms: None,
            preview: None,
        }
    }

    pub fn with_policy(
        mut self,
        revision: impl Into<String>,
        actor: Option<String>,
        ttl: Option<Duration>,
    ) -> Self {
        self.policy_revision = revision.into();
        self.actor = actor;
        self.expires_at_unix_ms = ttl.map(|ttl| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|now| now.as_millis() as u64 + ttl.as_millis() as u64)
                .unwrap_or(0)
        });
        self
    }

    pub fn with_preview(mut self, preview: Option<String>) -> Self {
        self.preview = preview;
        self
    }
}

pub fn digest_arguments(tool_name: &str, arguments: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\n");
    hasher.update(arguments.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

/// A persisted approval decision. It is reusable only for the same argument
/// digest and policy revision, and only until it expires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub effect_id: EffectId,
    pub run_id: RunId,
    pub call_id: String,
    pub tool_name: String,
    pub argument_digest: String,
    pub policy_revision: String,
    pub actor: Option<String>,
    pub decision: ApprovalDecision,
    pub decided_at_unix_ms: u64,
    pub expires_at_unix_ms: Option<u64>,
}

impl ApprovalRecord {
    pub fn from_request(
        request: &ApprovalRequest,
        decision: ApprovalDecision,
        decided_at_unix_ms: u64,
    ) -> Self {
        Self {
            effect_id: request.effect_id.clone(),
            run_id: RunId::new(request.run_id.clone()),
            call_id: request.call_id.clone(),
            tool_name: request.tool_name.clone(),
            argument_digest: request.argument_digest.clone(),
            policy_revision: request.policy_revision.clone(),
            actor: request.actor.clone(),
            decision,
            decided_at_unix_ms,
            expires_at_unix_ms: request.expires_at_unix_ms,
        }
    }

    /// Build a record from an effect and a tool call, for headless approval.
    pub fn from_effect(
        effect: &EffectRecord,
        run_id: &RunId,
        decision: ApprovalDecision,
        actor: Option<String>,
        policy_revision: impl Into<String>,
        ttl: Option<Duration>,
    ) -> Option<Self> {
        let intent: EffectIntent = serde_json::from_str(&effect.intent_json).ok()?;
        let call = intent.into_tool_call()?;
        let now = now_unix_ms();
        Some(Self {
            effect_id: effect.id.clone(),
            run_id: run_id.clone(),
            call_id: call.call_id,
            tool_name: call.name.clone(),
            argument_digest: digest_arguments(&call.name, &call.arguments),
            policy_revision: policy_revision.into(),
            actor,
            decision,
            decided_at_unix_ms: now,
            expires_at_unix_ms: ttl.map(|ttl| now + ttl.as_millis() as u64),
        })
    }

    pub fn is_usable_for(&self, request: &ApprovalRequest, now_unix_ms: u64) -> bool {
        self.effect_id == request.effect_id
            && self.argument_digest == request.argument_digest
            && self.policy_revision == request.policy_revision
            && self
                .expires_at_unix_ms
                .is_none_or(|expires| now_unix_ms < expires)
    }
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| now.as_millis() as u64)
        .unwrap_or(0)
}

pub use crate::error::ContractError as ApprovalStoreError;

/// Persistence for approval decisions. Kept separate from the effect journal
/// so a session/interaction store can implement it independently.
pub trait ApprovalStore: Send + Sync {
    fn save_approval(&self, record: &ApprovalRecord) -> Result<(), ApprovalStoreError>;
    fn load_approval(
        &self,
        effect_id: &EffectId,
    ) -> Result<Option<ApprovalRecord>, ApprovalStoreError>;
}

pub fn digest_intent(intent: &EffectIntent) -> String {
    let bytes = serde_json::to_vec(intent).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::digest_arguments;

    #[test]
    fn digest_is_stable_and_depends_on_tool_and_arguments() {
        let first = digest_arguments("write_file", r#"{"path":"a.txt"}"#);
        let second = digest_arguments("write_file", r#"{"path":"a.txt"}"#);
        let changed = digest_arguments("write_file", r#"{"path":"b.txt"}"#);

        assert_eq!(first, second);
        assert_ne!(first, changed);
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
    }
}

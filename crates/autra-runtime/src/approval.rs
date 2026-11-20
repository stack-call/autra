use async_trait::async_trait;
use autra_core::{EffectId, EffectIntent, RunId, ToolCallRequest};
use sha2::{Digest, Sha256};

pub const LOCAL_POLICY_REVISION: &str = "local-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub effect_id: EffectId,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
    pub argument_digest: String,
    pub policy_revision: String,
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
            preview: None,
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

pub(crate) fn digest_intent(intent: &EffectIntent) -> String {
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

//! Durable results are authoritative inputs; notifications only wake readers.
use autra_core::{BudgetSnapshot, EffectId, ModelOutput, ToolOutcome};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ResultIdentity {
    pub effect_id: EffectId,
    pub attempt_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectOutcome {
    Model(ModelOutput),
    ModelFailed(String),
    Tool(ToolOutcome),
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectResult {
    pub identity: ResultIdentity,
    pub outcome: EffectOutcome,
    #[serde(default)]
    pub budget: Option<BudgetSnapshot>,
}
/// Live Coordinator ownership and the optional Inbox record consumed at commit.
#[derive(Clone, Debug)]
pub struct CommitAuthority {
    pub owner_id: String,
    pub result: Option<ResultIdentity>,
}

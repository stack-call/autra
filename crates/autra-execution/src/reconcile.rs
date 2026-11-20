//! OutcomeUnknown reconciliation report.

use autra_core::{EffectId, RunId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedEffect {
    pub effect_id: EffectId,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub run_id: RunId,
    pub retried: Vec<EffectId>,
    pub unresolved: Vec<UnresolvedEffect>,
}

use autra_core::{RunId, ToolCallRequest};
use autra_execution::{PolicyDecision, PolicyRequest};

use super::LocalEngine;

impl LocalEngine {
    pub(super) fn policy_decision(
        &self,
        run_id: &RunId,
        call: &ToolCallRequest,
    ) -> Option<PolicyDecision> {
        self.tools.get(&call.name).map(|tool| {
            let contract = tool.contract();
            self.policy.decide(&PolicyRequest {
                run_id,
                tool_name: &call.name,
                arguments: &call.arguments,
                effect_class: contract.effect_class,
                requires_approval: contract.requires_approval,
            })
        })
    }

    pub(super) fn tool_needs_gate(&self, run_id: &RunId, call: &ToolCallRequest) -> bool {
        matches!(
            self.policy_decision(run_id, call),
            Some(PolicyDecision::Ask | PolicyDecision::Deny)
        )
    }
}

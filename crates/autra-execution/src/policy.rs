//! Tool policy evaluation.
//!
//! Policies may only narrow what the host already allows. A policy returning
//! `Allow` never lowers a baseline `Ask` or another policy's `Deny`.

use std::sync::Arc;

use autra_core::{EffectClass, RunId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PolicyDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Copy, Debug)]
pub struct PolicyRequest<'a> {
    pub run_id: &'a RunId,
    pub tool_name: &'a str,
    pub arguments: &'a str,
    pub effect_class: EffectClass,
    pub requires_approval: bool,
}

pub trait ToolPolicy: Send + Sync {
    fn revision(&self) -> &str;
    fn decide(&self, request: &PolicyRequest<'_>) -> PolicyDecision;
}

/// The host baseline: approval-required or unknown-effect tools need a
/// decision; everything else is allowed unless a policy says otherwise.
pub fn baseline_decision(request: &PolicyRequest<'_>) -> PolicyDecision {
    if request.requires_approval || request.effect_class == EffectClass::Unknown {
        PolicyDecision::Ask
    } else {
        PolicyDecision::Allow
    }
}

/// Ordered policies combined by severity. `Deny` wins over `Ask`, and `Ask`
/// wins over `Allow`, so a plugin cannot widen a trusted baseline.
#[derive(Clone, Default)]
pub struct PolicyChain {
    policies: Vec<Arc<dyn ToolPolicy>>,
}

impl PolicyChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(mut self, policy: impl ToolPolicy + 'static) -> Self {
        self.policies.push(Arc::new(policy));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    pub fn revision(&self) -> String {
        if self.policies.is_empty() {
            return crate::approval::LOCAL_POLICY_REVISION.to_owned();
        }
        self.policies
            .iter()
            .map(|policy| policy.revision())
            .collect::<Vec<_>>()
            .join("+")
    }

    pub fn decide(&self, request: &PolicyRequest<'_>) -> PolicyDecision {
        let mut decision = baseline_decision(request);
        for policy in &self.policies {
            decision = decision.max(policy.decide(request));
            if decision == PolicyDecision::Deny {
                break;
            }
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autra_core::{EffectClass, RunId};

    struct Fixed(PolicyDecision);

    impl ToolPolicy for Fixed {
        fn revision(&self) -> &str {
            "fixed"
        }

        fn decide(&self, _request: &PolicyRequest<'_>) -> PolicyDecision {
            self.0
        }
    }

    fn request<'a>(
        run_id: &'a RunId,
        effect_class: EffectClass,
        requires_approval: bool,
    ) -> PolicyRequest<'a> {
        PolicyRequest {
            run_id,
            tool_name: "tool",
            arguments: "{}",
            effect_class,
            requires_approval,
        }
    }

    #[test]
    fn baseline_asks_for_unknown_and_approval_required_tools() {
        let run_id = RunId::new("run");
        assert_eq!(
            baseline_decision(&request(&run_id, EffectClass::ReadOnly, false)),
            PolicyDecision::Allow
        );
        assert_eq!(
            baseline_decision(&request(&run_id, EffectClass::Unknown, false)),
            PolicyDecision::Ask
        );
        assert_eq!(
            baseline_decision(&request(&run_id, EffectClass::Idempotent, true)),
            PolicyDecision::Ask
        );
    }

    #[test]
    fn policies_can_only_narrow_the_baseline() {
        let run_id = RunId::new("run");
        let unknown = request(&run_id, EffectClass::Unknown, false);
        let read_only = request(&run_id, EffectClass::ReadOnly, false);

        assert_eq!(
            PolicyChain::new()
                .push(Fixed(PolicyDecision::Allow))
                .decide(&unknown),
            PolicyDecision::Ask
        );
        assert_eq!(
            PolicyChain::new()
                .push(Fixed(PolicyDecision::Deny))
                .decide(&read_only),
            PolicyDecision::Deny
        );
        assert_eq!(
            PolicyChain::new()
                .push(Fixed(PolicyDecision::Ask))
                .decide(&read_only),
            PolicyDecision::Ask
        );
    }
}

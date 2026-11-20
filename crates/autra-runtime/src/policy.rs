//! Concrete local policy rules for tool admission.

use std::collections::BTreeSet;

use autra_execution::{PolicyDecision, PolicyRequest, ToolPolicy};
use serde_json::Value;

/// A small rule policy for local hosts and the CLI.
///
/// Rules can only narrow the host baseline: the engine combines this with a
/// baseline that asks for approval-required and unknown-effect tools. Denied
/// commands are substring matches on `run_command`'s shell command, not a
/// sandbox.
#[derive(Clone, Default)]
pub struct RulePolicy {
    revision: String,
    ask_tools: BTreeSet<String>,
    deny_tools: BTreeSet<String>,
    deny_commands: Vec<String>,
}

impl RulePolicy {
    pub fn new(revision: impl Into<String>) -> Self {
        Self {
            revision: revision.into(),
            ..Self::default()
        }
    }

    pub fn ask_tool(mut self, name: impl Into<String>) -> Self {
        self.ask_tools.insert(name.into());
        self
    }

    pub fn deny_tool(mut self, name: impl Into<String>) -> Self {
        self.deny_tools.insert(name.into());
        self
    }

    pub fn deny_command(mut self, pattern: impl Into<String>) -> Self {
        self.deny_commands.push(pattern.into());
        self
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    fn denied_command(&self, arguments: &str) -> bool {
        if self.deny_commands.is_empty() {
            return false;
        }
        let Ok(value) = serde_json::from_str::<Value>(arguments) else {
            return false;
        };
        let Some(command) = value.get("command").and_then(Value::as_str) else {
            return false;
        };
        self.deny_commands
            .iter()
            .any(|pattern| command.contains(pattern))
    }
}

impl ToolPolicy for RulePolicy {
    fn revision(&self) -> &str {
        &self.revision
    }

    fn decide(&self, request: &PolicyRequest<'_>) -> PolicyDecision {
        if self.deny_tools.contains(request.tool_name) {
            return PolicyDecision::Deny;
        }
        if request.tool_name == "run_command" && self.denied_command(request.arguments) {
            return PolicyDecision::Deny;
        }
        if self.ask_tools.contains(request.tool_name) {
            return PolicyDecision::Ask;
        }
        PolicyDecision::Allow
    }
}

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use autra_core::{BudgetSnapshot, EffectClass, RunId, ToolSpec};
use autra_harness::{Tool, ToolContext, ToolError, ToolInput, ToolOutput};
use serde::Deserialize;
use serde_json::json;

use crate::{Agent, RunStatus};

#[derive(Clone)]
pub struct Subagent {
    pub description: String,
    pub agent: Arc<Agent>,
}

#[derive(Clone, Default)]
pub struct SubagentRegistry {
    entries: BTreeMap<String, Subagent>,
}

impl SubagentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        agent: Agent,
    ) -> &mut Self {
        self.entries.insert(
            name.into(),
            Subagent {
                description: description.into(),
                agent: Arc::new(agent),
            },
        );
        self
    }

    pub fn get(&self, name: &str) -> Option<&Subagent> {
        self.entries.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    pub fn descriptions(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .map(|(name, subagent)| (name.as_str(), subagent.description.as_str()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub struct SubagentTool {
    registry: SubagentRegistry,
}

impl SubagentTool {
    pub fn new(registry: SubagentRegistry) -> Self {
        Self { registry }
    }
}

#[derive(Deserialize)]
struct SpawnArgs {
    agent: String,
    task: String,
}

#[async_trait]
impl Tool for SubagentTool {
    fn spec(&self) -> ToolSpec {
        let names = self.registry.names();
        let listed = self
            .registry
            .descriptions()
            .iter()
            .map(|(name, description)| format!("{name}: {description}"))
            .collect::<Vec<_>>()
            .join("; ");
        let input_schema = json!({
            "type": "object",
            "properties": {
                "agent": { "type": "string", "enum": names },
                "task": { "type": "string", "description": "A self-contained task for the subagent" }
            },
            "required": ["agent", "task"]
        })
        .to_string();
        ToolSpec {
            name: "spawn_subagent".to_owned(),
            description: format!("Run a named subagent ({listed})."),
            input_schema,
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::NonIdempotent
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let run_id = RunId::fresh();
        let context = ToolContext {
            environment: None,
            run_id: run_id.clone(),
            call_id: "direct-subagent".to_owned(),
            parent_run_id: Some(run_id),
            cancel: None,
            budget: None,
            child_execution: None,
        };
        self.execute(
            ToolInput {
                arguments: arguments.to_owned(),
            },
            context,
        )
        .await
        .map(|output| output.content)
    }

    async fn execute(
        &self,
        input: ToolInput,
        context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SpawnArgs = serde_json::from_str(&input.arguments).map_err(|error| {
            ToolError::new(format!("invalid spawn_subagent arguments: {error}"))
        })?;
        let subagent = self
            .registry
            .get(&args.agent)
            .ok_or_else(|| ToolError::new(format!("unknown subagent `{}`", args.agent)))?;
        let parent_run_id = context
            .parent_run_id
            .clone()
            .unwrap_or_else(|| context.run_id.clone());
        let cancel = context.cancel.clone().unwrap_or_default();
        // A child Run has its own budget, carved out of the parent's so a
        // subagent cannot silently amplify the conversation's allowance.
        let depth = context
            .child_execution
            .as_ref()
            .map_or(0, |execution| execution.depth);
        if depth >= 1 {
            return Err(ToolError::new(
                "subagent nesting limit reached: a subagent cannot spawn another subagent",
            ));
        }
        let child_budget = match context.budget.as_ref() {
            Some(budget) => {
                let requested = 4_u32.min(budget.remaining_tool_calls());
                budget.reserve_child(4, requested).ok_or_else(|| {
                    ToolError::new("parent budget exhausted: cannot dedicate a child budget")
                })?
            }
            None => BudgetSnapshot::new(4, 4),
        };
        let mut execution = context.child_execution.clone().unwrap_or_default();
        execution.depth = depth + 1;
        let outcome = subagent
            .agent
            .run_as_child_with(
                parent_run_id,
                cancel,
                args.task,
                child_budget,
                execution,
                false,
            )
            .await
            .map_err(|error| {
                ToolError::new(format!("subagent `{}` failed: {error}", args.agent))
            })?;
        if outcome.status() == RunStatus::Cancelled {
            return Err(ToolError::new(format!(
                "subagent `{}` was cancelled",
                args.agent
            )));
        }
        Ok(ToolOutput::text(
            outcome
                .answer()
                .map(str::to_owned)
                .unwrap_or_else(|| "(no answer)".to_owned()),
        ))
    }
}

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use autra_harness::{Tool, ToolError};
use serde::Deserialize;
use serde_json::json;

use crate::Agent;

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
        let args: SpawnArgs = serde_json::from_str(arguments).map_err(|error| {
            ToolError::new(format!("invalid spawn_subagent arguments: {error}"))
        })?;
        let subagent = self
            .registry
            .get(&args.agent)
            .ok_or_else(|| ToolError::new(format!("unknown subagent `{}`", args.agent)))?;
        let outcome = subagent.agent.run(&args.task).await.map_err(|error| {
            ToolError::new(format!("subagent `{}` failed: {error}", args.agent))
        })?;
        Ok(outcome
            .answer()
            .map(str::to_owned)
            .unwrap_or_else(|| "(no answer)".to_owned()))
    }
}

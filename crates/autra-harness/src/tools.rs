use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use serde::Deserialize;
use thiserror::Error;

/// A capability the model may request.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn effect_class(&self) -> EffectClass {
        EffectClass::Unknown
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError>;
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: impl Tool + 'static) -> &mut Self {
        self.tools.insert(tool.spec().name, Arc::new(tool));
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Returns its text argument unchanged. Useful for deterministic tests.
pub struct EchoTool;

#[derive(Deserialize)]
struct EchoArgs {
    text: String,
}

const ECHO_SCHEMA: &str = r#"{"type":"object","properties":{"text":{"type":"string","description":"Text to return unchanged"}},"required":["text"]}"#;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".to_owned(),
            description: "Return the provided text unchanged.".to_owned(),
            input_schema: ECHO_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: EchoArgs = serde_json::from_str(arguments)
            .map_err(|error| ToolError::new(format!("invalid echo arguments: {error}")))?;
        Ok(args.text)
    }
}

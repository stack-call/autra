use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use autra_core::{CancelFlag, EffectClass, RunId, ToolSpec};
use thiserror::Error;

pub const TOOL_PROTOCOL_VERSION: u32 = 1;

/// Model-facing tool description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

impl From<ToolSpec> for ToolDescriptor {
    fn from(spec: ToolSpec) -> Self {
        Self {
            name: spec.name,
            description: spec.description,
            input_schema: spec.input_schema,
        }
    }
}

/// Execution governance for a tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolContract {
    pub effect_class: EffectClass,
    pub requires_approval: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInput {
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    /// The Run that invoked the tool.
    pub run_id: RunId,
    pub call_id: String,
    /// The current Run, for tools that spawn child work.
    pub parent_run_id: Option<RunId>,
    /// Cancellation of the invoking scope, for tools that spawn child work.
    pub cancel: Option<CancelFlag>,
}

/// A capability the model may request.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn effect_class(&self) -> EffectClass {
        EffectClass::Unknown
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn descriptor(&self) -> ToolDescriptor {
        self.spec().into()
    }

    fn contract(&self) -> ToolContract {
        ToolContract {
            effect_class: self.effect_class(),
            requires_approval: self.requires_approval(),
        }
    }

    fn protocol_version(&self) -> u32 {
        TOOL_PROTOCOL_VERSION
    }

    /// A human-readable description of the pending change, shown to whoever
    /// approves the call. Returning `None` leaves only the raw arguments.
    fn preview(&self, _arguments: &str) -> Option<String> {
        None
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError>;

    /// Execution entry point used by the runtime. Existing tools get this
    /// adapter for free; richer tools can override it to use the context.
    async fn execute(
        &self,
        input: ToolInput,
        _context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        self.call(&input.arguments).await.map(ToolOutput::text)
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ToolError {
    pub message: String,
    pub outcome_unknown: bool,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome_unknown: false,
        }
    }

    /// The tool may have changed external state before failing.
    pub fn outcome_unknown(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome_unknown: true,
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

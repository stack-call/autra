//! Rich harness: composable agent behaviour built on `autra-core`.
//!
//! MVP contents: the default ReAct stepper, the model-provider boundary, the
//! tool contract and workspace-scoped read-only tools.

pub mod model;
pub mod react;
pub mod tools;

pub use autra_core::{HistoryItem, ModelCallSpec, ModelOutput, ToolCallRequest, ToolOutcome};
pub use model::{EchoModel, FakeModel, ModelError, ModelProvider};
pub use react::ReActStepper;
pub use tools::{
    EchoTool, ListFilesTool, LocalWorkspace, ReadFileTool, Tool, ToolError, ToolRegistry,
    WriteFileTool,
};

//! Rich harness: composable agent behaviour built on `autra-core`.
//!
//! MVP contents: the default ReAct stepper, the model-provider boundary, the
//! tool contract and workspace-scoped read-only tools.

#[cfg(feature = "process")]
pub mod command;
pub mod context;
#[cfg(feature = "process")]
pub mod git;
pub mod middleware;
pub mod model;
pub mod planning;
pub mod react;
pub mod skills;
pub mod tools;

pub use autra_core::{HistoryItem, ModelCallSpec, ModelOutput, ToolCallRequest, ToolOutcome};
#[cfg(feature = "process")]
pub use command::RunCommandTool;
pub use context::{
    ContextBuilder, ContextError, ContextRequest, SimpleContextBuilder, SkillSummary,
};
#[cfg(feature = "process")]
pub use git::{GitDiffTool, GitStatusTool};
pub use middleware::{
    ExtensionError, MiddlewareStepper, ModelBuildContext, ModelMiddleware, ModelRequestDraft,
    ModelRequestTransform,
};
pub use model::{
    EchoModel, FakeModel, MODEL_PROTOCOL_VERSION, ModelCapabilities, ModelError, ModelProvider,
};
pub use planning::{PlanItem, PlanStatus, SharedPlan, UpdatePlanTool};
pub use react::{ReActStepper, react_engine_state, react_history, react_pending_tool_calls};
pub use skills::{LoadSkillTool, Skill, SkillRegistry};
pub use tools::{
    EchoTool, EditFileTool, ListFilesTool, LocalWorkspace, ReadFileTool, SearchTextTool,
    TOOL_PROTOCOL_VERSION, Tool, ToolContext, ToolContract, ToolDescriptor, ToolError, ToolInput,
    ToolOutput, ToolRegistry, WriteFileTool,
};

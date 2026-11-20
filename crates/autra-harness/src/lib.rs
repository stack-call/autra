//! Rich harness: composable agent behaviour built on `autra-core`.
//!
//! MVP contents: the default ReAct stepper, the model-provider boundary, the
//! tool contract and workspace-scoped read-only tools.

pub mod context;
pub mod model;
pub mod planning;
pub mod react;
pub mod skills;
pub mod tools;

pub use autra_core::{HistoryItem, ModelCallSpec, ModelOutput, ToolCallRequest, ToolOutcome};
pub use context::{
    ContextBuilder, ContextError, ContextRequest, SimpleContextBuilder, SkillSummary,
};
pub use model::{EchoModel, FakeModel, ModelError, ModelProvider};
pub use planning::{PlanItem, PlanStatus, SharedPlan, UpdatePlanTool};
pub use react::ReActStepper;
pub use skills::{LoadSkillTool, Skill, SkillRegistry};
pub use tools::{
    EchoTool, ListFilesTool, LocalWorkspace, ReadFileTool, Tool, ToolError, ToolRegistry,
    WriteFileTool,
};

//! Rich harness: composable agent behaviour built on `autra-core`.
//!
//! MVP contents: the default ReAct stepper, the model-provider boundary and
//! the tool contract.

pub mod model;
pub mod react;
pub mod tools;

pub use model::{EchoModel, FakeModel, ModelError, ModelProvider};
pub use react::ReActStepper;
pub use tools::{EchoTool, Tool, ToolError, ToolRegistry};

//! Rich harness: composable agent behaviour built on `autra-core`.
//!
//! MVP contents: the default ReAct stepper and the model-provider boundary.

pub mod model;
pub mod react;

pub use model::{EchoModel, ModelError, ModelProvider};
pub use react::ReActStepper;

use async_trait::async_trait;
use autra_core::{ModelCallSpec, ModelOutput};
use thiserror::Error;

pub const MODEL_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tool_calling: bool,
    pub streaming: bool,
    pub structured_output: bool,
    pub multimodal_input: bool,
    pub max_context_tokens: Option<u32>,
}

/// The boundary between the harness and a model provider.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError>;

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn protocol_version(&self) -> u32 {
        MODEL_PROTOCOL_VERSION
    }
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("{0}")]
    Message(String),
}

impl ModelError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

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

/// Emit a complete output as a single delta, for providers without streaming.
pub fn emit_complete_text(output: &ModelOutput, on_delta: &mut (dyn FnMut(String) + Send)) {
    if let Some(text) = output.text.clone() {
        on_delta(text);
    }
}

/// The boundary between the harness and a model provider.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError>;

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    /// Stable identity used by recovery profiles. It must not include secrets.
    fn profile_id(&self) -> Option<String> {
        None
    }

    fn protocol_version(&self) -> u32 {
        MODEL_PROTOCOL_VERSION
    }

    /// Streaming variant. The default emits the complete text once, so every
    /// provider works without a streaming implementation.
    async fn complete_streaming(
        &self,
        request: &ModelCallSpec,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, ModelError> {
        let output = self.complete(request).await?;
        emit_complete_text(&output, on_delta);
        Ok(output)
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

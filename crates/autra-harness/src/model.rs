use async_trait::async_trait;
use autra_core::{HistoryItem, ModelCallSpec, ModelOutput};
use thiserror::Error;

/// The boundary between the harness and a model provider.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError>;
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

/// Returns the last user input prefixed with `echo:`.
#[derive(Clone, Copy, Debug, Default)]
pub struct EchoModel;

#[async_trait]
impl ModelProvider for EchoModel {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        let input = request
            .messages
            .iter()
            .rev()
            .find_map(|item| match item {
                HistoryItem::User(text) => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("");
        Ok(ModelOutput {
            text: Some(format!("echo: {input}")),
            tool_calls: Vec::new(),
        })
    }
}

/// A deterministic provider that replays scripted responses in order.
#[derive(Default)]
pub struct FakeModel {
    responses: std::sync::Mutex<std::collections::VecDeque<ModelOutput>>,
}

impl FakeModel {
    pub fn new(responses: impl IntoIterator<Item = ModelOutput>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().collect()),
        }
    }

    pub fn push(&self, response: ModelOutput) {
        self.responses
            .lock()
            .expect("fake model lock poisoned")
            .push_back(response);
    }
}

#[async_trait]
impl ModelProvider for FakeModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.responses
            .lock()
            .expect("fake model lock poisoned")
            .pop_front()
            .ok_or_else(|| ModelError::new("fake model script exhausted"))
    }
}

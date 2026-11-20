//! An OpenAI-compatible chat completions provider.
//!
//! The provider is an adapter behind `autra_harness::ModelProvider`; it does
//! not change core semantics. Non-streaming and SSE streaming completions are
//! supported.

mod mapping;

use async_trait::async_trait;
use autra_core::{ModelCallSpec, ModelOutput};
use autra_harness::{ModelCapabilities, ModelError, ModelProvider};
use futures_util::StreamExt;
use thiserror::Error;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone)]
pub struct OpenAiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl OpenAiConfig {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    pub fn from_env(model: impl Into<String>) -> Result<Self, ConfigError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| ConfigError::MissingApiKey)?;
        let base_url =
            std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
        Ok(Self {
            base_url,
            api_key,
            model: model.into(),
        })
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("OPENAI_API_KEY is not set")]
    MissingApiKey,
}

pub struct OpenAiCompatibleModel {
    client: reqwest::Client,
    config: OpenAiConfig,
}

impl OpenAiCompatibleModel {
    pub fn new(config: OpenAiConfig) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| ModelError::new(format!("cannot build HTTP client: {error}")))?;
        Ok(Self { client, config })
    }

    pub fn from_env(model: impl Into<String>) -> Result<Self, ModelError> {
        let config =
            OpenAiConfig::from_env(model).map_err(|error| ModelError::new(error.to_string()))?;
        Self::new(config)
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            tool_calling: true,
            streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        let body = mapping::build_request(&self.config.model, request, false)?;
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| ModelError::new(format!("model request failed: {error}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| ModelError::new(format!("cannot read model response: {error}")))?;
        if !status.is_success() {
            return Err(ModelError::new(format!(
                "model request failed with {status}: {}",
                mapping::truncate(&text, 500)
            )));
        }
        let value = serde_json::from_str(&text).map_err(|error| {
            ModelError::new(format!(
                "invalid model response JSON: {error}: {}",
                mapping::truncate(&text, 500)
            ))
        })?;
        mapping::parse_response(&value)
    }

    async fn complete_streaming(
        &self,
        request: &ModelCallSpec,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, ModelError> {
        let body = mapping::build_request(&self.config.model, request, true)?;
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| ModelError::new(format!("model request failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ModelError::new(format!(
                "model request failed with {status}: {}",
                mapping::truncate(&text, 500)
            )));
        }
        let mut accumulator = mapping::StreamAccumulator::default();
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| ModelError::new(format!("model stream failed: {error}")))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(index) = buffer.find('\n') {
                let line: String = buffer[..index].trim_end_matches('\r').to_owned();
                buffer.drain(..=index);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(mapping::finish_stream(accumulator));
                }
                if let Some(delta) = mapping::apply_stream_data(data, &mut accumulator)? {
                    on_delta(delta);
                }
            }
        }
        Ok(mapping::finish_stream(accumulator))
    }
}

impl std::fmt::Debug for OpenAiConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("model", &self.model)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::OpenAiConfig;

    #[test]
    fn debug_redacts_api_key() {
        let config = OpenAiConfig::new("http://localhost", "secret-key", "model");

        let debug = format!("{config:?}");

        assert!(!debug.contains("secret-key"));
        assert!(debug.contains("[redacted]"));
    }
}

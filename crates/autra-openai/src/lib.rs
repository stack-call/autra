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
            .map_err(|error| {
                ModelError::new(format!("model request failed: {}", describe(&error)))
            })?;
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
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                ModelError::new(format!("model request failed: {}", describe(&error)))
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ModelError::new(format!(
                "model request failed with {status}: {}",
                mapping::truncate(&text, 500)
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_ascii_lowercase());
        let looks_like_json = content_type
            .as_deref()
            .is_some_and(|value| value.contains("application/json") || value.contains("+json"));
        if looks_like_json {
            // Some gateways ignore `stream: true` and answer with a complete
            // JSON body. Accept it rather than reporting an empty stream.
            let text = response
                .text()
                .await
                .map_err(|error| ModelError::new(format!("model response failed: {error}")))?;
            let output = mapping::parse_response_body(&text)?;
            if let Some(text) = &output.text {
                on_delta(text.clone());
            }
            return Ok(output);
        }
        let mut accumulator = mapping::StreamAccumulator::default();
        let mut decoder = mapping::SseDecoder::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| ModelError::new(format!("model stream failed: {error}")))?;
            let mut events = Vec::new();
            decoder.push(&chunk, &mut events)?;
            for data in events {
                if data == "[DONE]" {
                    return Ok(mapping::finish_stream(accumulator));
                }
                if let Some(delta) = mapping::apply_stream_data(&data, &mut accumulator)? {
                    on_delta(delta);
                }
            }
        }
        let mut events = Vec::new();
        decoder.finish(&mut events)?;
        for data in events {
            if data == "[DONE]" {
                return Ok(mapping::finish_stream(accumulator));
            }
            if let Some(delta) = mapping::apply_stream_data(&data, &mut accumulator)? {
                on_delta(delta);
            }
        }
        Ok(mapping::finish_stream(accumulator))
    }
}

fn describe(error: &reqwest::Error) -> String {
    use std::error::Error;

    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
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

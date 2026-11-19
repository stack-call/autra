//! An OpenAI-compatible chat completions provider.
//!
//! The provider is an adapter behind `autra_harness::ModelProvider`; it does
//! not change core semantics. Only non-streaming completions are implemented.

mod mapping;

use std::time::Duration;

use async_trait::async_trait;
use autra_core::{ModelCallSpec, ModelOutput};
use autra_harness::{ModelError, ModelProvider};
use thiserror::Error;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone, Debug)]
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
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| ModelError::new(format!("cannot build HTTP client: {error}")))?;
        Ok(Self { client, config })
    }

    pub fn from_env(model: impl Into<String>) -> Result<Self, ModelError> {
        let config =
            OpenAiConfig::from_env(model).map_err(|error| ModelError::new(error.to_string()))?;
        Self::new(config)
    }

    pub fn config(&self) -> &OpenAiConfig {
        &self.config
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatibleModel {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        let body = mapping::build_request(&self.config.model, request)?;
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
        let value = serde_json::from_str(&text)
            .map_err(|error| ModelError::new(format!("invalid model response JSON: {error}")))?;
        mapping::parse_response(&value)
    }
}

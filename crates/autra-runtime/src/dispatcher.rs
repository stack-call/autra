//! Effect execution boundary. The Agent calls a Dispatcher instead of model and
//! tools directly; the outbox dispatcher loop is a future host concern.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use autra_core::{ModelCallSpec, ModelOutput, RunId, ToolCallRequest, ToolOutcome};
use autra_harness::{ModelError, ModelProvider, ToolContext, ToolInput, ToolRegistry};
use futures_util::FutureExt;

/// Executes accepted effect intents. A custom Dispatcher can wrap a provider,
/// a sandbox or a remote worker.
#[async_trait]
pub trait Dispatcher: Send + Sync {
    async fn call_model(
        &self,
        request: &ModelCallSpec,
        timeout: Option<Duration>,
    ) -> Result<ModelOutput, ModelError>;

    async fn call_tool(
        &self,
        run_id: &RunId,
        call: &ToolCallRequest,
        timeout: Option<Duration>,
    ) -> ToolOutcome;
}

pub struct LocalDispatcher {
    model: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
}

impl LocalDispatcher {
    pub fn new(model: Arc<dyn ModelProvider>, tools: ToolRegistry) -> Self {
        Self { model, tools }
    }
}

#[async_trait]
impl Dispatcher for LocalDispatcher {
    async fn call_model(
        &self,
        request: &ModelCallSpec,
        timeout: Option<Duration>,
    ) -> Result<ModelOutput, ModelError> {
        match with_timeout(timeout, self.model.complete(request)).await {
            Ok(result) => result,
            Err(()) => Err(ModelError::new(
                "model call exceeded the configured timeout",
            )),
        }
    }

    async fn call_tool(
        &self,
        run_id: &RunId,
        call: &ToolCallRequest,
        timeout: Option<Duration>,
    ) -> ToolOutcome {
        execute_tool(self.tools.clone(), call.clone(), timeout, run_id.clone()).await
    }
}

async fn with_timeout<F, T, E>(timeout: Option<Duration>, future: F) -> Result<Result<T, E>, ()>
where
    F: Future<Output = Result<T, E>>,
{
    match timeout {
        Some(duration) => tokio::time::timeout(duration, future).await.map_err(|_| ()),
        None => Ok(future.await),
    }
}

async fn execute_tool(
    tools: ToolRegistry,
    call: ToolCallRequest,
    timeout: Option<Duration>,
    run_id: RunId,
) -> ToolOutcome {
    let result: Result<String, (String, bool)> = match tools.get(&call.name) {
        Some(tool) => {
            let input = ToolInput {
                arguments: call.arguments.clone(),
            };
            let context = ToolContext {
                run_id,
                call_id: call.call_id.clone(),
            };
            let future = AssertUnwindSafe(with_timeout(timeout, tool.execute(input, context)));
            match future.catch_unwind().await {
                Ok(Ok(Ok(output))) => Ok(output.content),
                Ok(Ok(Err(error))) => Err((error.to_string(), false)),
                Ok(Err(())) => Err((
                    format!(
                        "tool `{}` timed out; it may still have run and its outcome is unknown",
                        call.name
                    ),
                    true,
                )),
                Err(_) => Err((
                    format!(
                        "tool `{}` panicked; it may have run and its outcome is unknown",
                        call.name
                    ),
                    true,
                )),
            }
        }
        None => Err((format!("unknown tool `{}`", call.name), false)),
    };
    let (content, is_error, outcome_unknown) = match result {
        Ok(content) => (content, false, false),
        Err((message, unknown)) => (message, true, unknown),
    };
    ToolOutcome {
        call_id: call.call_id,
        name: call.name,
        content,
        is_error,
        outcome_unknown,
    }
}

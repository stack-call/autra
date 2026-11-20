use std::time::Duration;

use async_trait::async_trait;
use autra::{Agent, Dispatcher};
use autra_core::{ModelCallSpec, ModelOutput, RunId, ToolCallRequest, ToolOutcome};
use autra_harness::ModelError;

struct ConstantDispatcher;

#[async_trait]
impl Dispatcher for ConstantDispatcher {
    async fn call_model(
        &self,
        _request: &ModelCallSpec,
        _timeout: Option<Duration>,
    ) -> Result<ModelOutput, ModelError> {
        Ok(ModelOutput {
            text: Some("custom dispatcher".to_owned()),
            tool_calls: Vec::new(),
        })
    }

    async fn call_tool(
        &self,
        _run_id: &RunId,
        call: &ToolCallRequest,
        _timeout: Option<Duration>,
    ) -> ToolOutcome {
        ToolOutcome {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: "custom tool result".to_owned(),
            is_error: false,
            outcome_unknown: false,
        }
    }
}

#[tokio::test]
async fn a_custom_dispatcher_replaces_the_local_executor() {
    let agent = Agent::builder("dispatcher")
        .dispatcher(ConstantDispatcher)
        .build()
        .unwrap();

    let outcome = agent.run("hello").await.unwrap();

    assert_eq!(outcome.answer(), Some("custom dispatcher"));
}

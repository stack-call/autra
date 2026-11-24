use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra::Agent;
use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, Disposition, EngineState, EventKind,
    ModelCallSpec, ModelOutput, StepError, Transition,
};
use autra_harness::{
    ContextBuilder, ContextError, ContextRequest, EchoModel, ModelError, ModelProvider,
};

struct CustomContext;

impl ContextBuilder for CustomContext {
    fn build(&self, request: ContextRequest<'_>) -> Result<ModelCallSpec, ContextError> {
        Ok(ModelCallSpec {
            system: Some("custom context".to_owned()),
            messages: request.history.to_vec(),
            tools: request.tools.to_vec(),
        })
    }
}

struct CapturingModel {
    seen: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl ModelProvider for CapturingModel {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.seen
            .lock()
            .expect("system log poisoned")
            .push(request.system.clone());
        Ok(ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        })
    }
}

#[tokio::test]
async fn custom_context_builder_is_used() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder("context")
        .model(CapturingModel { seen: seen.clone() })
        .context_builder(CustomContext)
        .build()
        .unwrap();

    let outcome = agent.run("hello").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    assert_eq!(seen.lock().unwrap()[0].as_deref(), Some("custom context"));
}

struct OneShotStepper;

impl AgentStepper for OneShotStepper {
    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<Transition, StepError> {
        match &event.kind {
            EventKind::RunStarted { input: _ } => {
                let mut next = state.clone();
                next.phase = AgentPhase::Completed;
                next.final_answer = Some("custom stepper".to_owned());
                next.engine_state = EngineState::new("one-shot", 1)
                    .with_data(serde_json::json!({ "stage": "done" }));
                Ok(Transition {
                    next,
                    intents: Vec::new(),
                    disposition: Disposition::Finished,
                    engine_delta: None,
                })
            }
            _ => Err(StepError::new("unexpected event")),
        }
    }
}

#[tokio::test]
async fn custom_stepper_is_used() {
    let agent = Agent::builder("engine")
        .model(EchoModel)
        .stepper(OneShotStepper)
        .build()
        .unwrap();

    let outcome = agent.run("hello").await.unwrap();

    assert_eq!(outcome.answer(), Some("custom stepper"));
    assert_eq!(outcome.state().engine_state.engine_id, "one-shot");
    assert_eq!(
        outcome.state().engine_state.data,
        serde_json::json!({ "stage": "done" })
    );
}

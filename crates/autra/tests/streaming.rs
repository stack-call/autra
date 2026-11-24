use async_trait::async_trait;
use autra::{Agent, RunEvent};
use autra_core::ModelCallSpec;
use autra_harness::{EchoModel, ModelCapabilities, ModelError, ModelOutput, ModelProvider};

#[derive(Clone, Copy)]
struct StreamingModel;

#[async_trait]
impl ModelProvider for StreamingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        Ok(ModelOutput {
            usage: None,
            text: Some("hello".to_owned()),
            tool_calls: Vec::new(),
        })
    }

    async fn complete_streaming(
        &self,
        _request: &ModelCallSpec,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, ModelError> {
        on_delta("he".to_owned());
        on_delta("llo".to_owned());
        Ok(ModelOutput {
            usage: None,
            text: Some("hello".to_owned()),
            tool_calls: Vec::new(),
        })
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            ..ModelCapabilities::default()
        }
    }
}

#[tokio::test]
async fn deltas_arrive_before_the_model_result() {
    let agent = Agent::builder("stream")
        .model(StreamingModel)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", Default::default(), |event| events.push(event.clone()))
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("hello"));
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::ModelDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.concat(), "hello");
    assert!(deltas.len() <= 2, "deltas should be coalesced");
    let last_delta = events
        .iter()
        .rposition(|event| matches!(event, RunEvent::ModelDelta { .. }))
        .unwrap();
    let result = events
        .iter()
        .position(|event| matches!(event, RunEvent::ModelResult { .. }))
        .unwrap();
    assert!(last_delta < result);
}

#[tokio::test]
async fn a_non_streaming_provider_emits_one_delta_by_default() {
    let agent = Agent::builder("stream").model(EchoModel).build().unwrap();
    let mut events = Vec::new();

    agent
        .run_with_options("hi", Default::default(), |event| events.push(event.clone()))
        .await
        .unwrap();

    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::ModelDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["echo: hi"]);
}

#[tokio::test]
async fn a_journaled_run_streams_deltas_through_the_outbox() {
    let journal = autra::MemoryJournal::new();
    let agent = Agent::builder("stream")
        .model(StreamingModel)
        .journal(journal)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", Default::default(), |event| events.push(event.clone()))
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("hello"));
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::ModelDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.concat(), "hello");
    assert!(deltas.len() <= 2, "deltas should be coalesced");
    let last_delta = events
        .iter()
        .rposition(|event| matches!(event, RunEvent::ModelDelta { .. }))
        .unwrap();
    let result = events
        .iter()
        .position(|event| matches!(event, RunEvent::ModelResult { .. }))
        .unwrap();
    assert!(last_delta < result);
}

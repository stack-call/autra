use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra::{Agent, RunError};
use autra_core::{ModelCallSpec, ModelOutput};
use autra_harness::{
    EchoTool, ExtensionError, ModelBuildContext, ModelError, ModelProvider, ModelRequestDraft,
    ModelRequestTransform,
};

#[derive(Clone, Default)]
struct CapturingModel {
    requests: Arc<Mutex<Vec<ModelCallSpec>>>,
}

#[async_trait]
impl ModelProvider for CapturingModel {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        })
    }
}

struct AddSystem(&'static str);

impl ModelRequestTransform for AddSystem {
    fn transform(
        &self,
        _ctx: &ModelBuildContext,
        mut draft: ModelRequestDraft,
    ) -> Result<ModelRequestDraft, ExtensionError> {
        draft.system = Some(match draft.system {
            Some(existing) => format!("{existing}\n{}", self.0),
            None => self.0.to_owned(),
        });
        Ok(draft)
    }
}

struct DropTools;

impl ModelRequestTransform for DropTools {
    fn transform(
        &self,
        _ctx: &ModelBuildContext,
        mut draft: ModelRequestDraft,
    ) -> Result<ModelRequestDraft, ExtensionError> {
        draft.tools.clear();
        Ok(draft)
    }
}

struct TurnAware;

impl ModelRequestTransform for TurnAware {
    fn transform(
        &self,
        ctx: &ModelBuildContext,
        mut draft: ModelRequestDraft,
    ) -> Result<ModelRequestDraft, ExtensionError> {
        draft.system = Some(format!("turn={}", ctx.turn));
        Ok(draft)
    }
}

struct Failing;

impl ModelRequestTransform for Failing {
    fn transform(
        &self,
        _ctx: &ModelBuildContext,
        _draft: ModelRequestDraft,
    ) -> Result<ModelRequestDraft, ExtensionError> {
        Err(ExtensionError::new("middleware refused the request"))
    }
}

#[tokio::test]
async fn middleware_changes_the_uncommitted_model_request() {
    let model = CapturingModel::default();
    let agent = Agent::builder("mw")
        .model(model.clone())
        .tool(EchoTool)
        .middleware(AddSystem("guard"))
        .middleware(DropTools)
        .build()
        .unwrap();

    agent.run("go").await.unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].system.as_deref(), Some("guard"));
    assert!(requests[0].tools.is_empty());
}

#[tokio::test]
async fn transforms_run_in_registration_order() {
    let model = CapturingModel::default();
    let agent = Agent::builder("mw")
        .model(model.clone())
        .middleware(AddSystem("first"))
        .middleware(AddSystem("second"))
        .build()
        .unwrap();

    agent.run("go").await.unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests[0].system.as_deref(), Some("first\nsecond"));
}

#[tokio::test]
async fn middleware_sees_read_only_run_facts() {
    let model = CapturingModel::default();
    let agent = Agent::builder("mw")
        .model(model.clone())
        .middleware(TurnAware)
        .build()
        .unwrap();

    agent.run("go").await.unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests[0].system.as_deref(), Some("turn=1"));
}

#[tokio::test]
async fn a_middleware_error_fails_the_run_before_the_model_call() {
    let model = CapturingModel::default();
    let agent = Agent::builder("mw")
        .model(model.clone())
        .middleware(Failing)
        .build()
        .unwrap();

    let error = agent.run("go").await.unwrap_err();

    assert!(matches!(error, RunError::Step(_)));
    assert!(model.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_middleware_the_request_is_unchanged() {
    let model = CapturingModel::default();
    let agent = Agent::builder("mw")
        .model(model.clone())
        .tool(EchoTool)
        .build()
        .unwrap();

    agent.run("go").await.unwrap();

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests[0].tools.len(), 1);
    assert!(requests[0].system.is_none());
}

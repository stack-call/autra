//! Typed model-request middleware.
//!
//! Transforms see a restricted draft of the next model request, never the
//! authoritative Run state or a committed Effect.

use std::sync::Arc;

use autra_core::{
    AcceptedEvent, AgentState, AgentStepper, EffectIntent, HistoryItem, ModelCallSpec, RunId,
    StepError, ToolCallRequest, ToolSpec, Transition,
};

pub use autra_execution::ContractError as ExtensionError;

/// Read-only facts about the request being built.
#[derive(Clone, Debug)]
pub struct ModelBuildContext {
    pub run_id: RunId,
    pub turn: u32,
}

/// The mutable part a transform may change. It is not `AgentState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRequestDraft {
    pub system: Option<String>,
    pub messages: Vec<HistoryItem>,
    pub tools: Vec<ToolSpec>,
}

impl ModelRequestDraft {
    pub fn from_spec(spec: ModelCallSpec) -> Self {
        Self {
            system: spec.system,
            messages: spec.messages,
            tools: spec.tools,
        }
    }

    pub fn into_spec(self) -> ModelCallSpec {
        ModelCallSpec {
            system: self.system,
            messages: self.messages,
            tools: self.tools,
        }
    }
}

/// One ordered transform over an uncommitted model request.
pub trait ModelRequestTransform: Send + Sync {
    fn transform(
        &self,
        ctx: &ModelBuildContext,
        draft: ModelRequestDraft,
    ) -> Result<ModelRequestDraft, ExtensionError>;
}

/// Transforms applied in registration order.
#[derive(Clone, Default)]
pub struct ModelMiddleware {
    transforms: Vec<Arc<dyn ModelRequestTransform>>,
}

impl ModelMiddleware {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(mut self, transform: impl ModelRequestTransform + 'static) -> Self {
        self.transforms.push(Arc::new(transform));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.transforms.is_empty()
    }

    pub fn apply(
        &self,
        ctx: &ModelBuildContext,
        draft: ModelRequestDraft,
    ) -> Result<ModelRequestDraft, ExtensionError> {
        let mut draft = draft;
        for transform in &self.transforms {
            draft = transform.transform(ctx, draft)?;
        }
        Ok(draft)
    }
}

/// Wraps an engine so every accepted `CallModel` intent passes through the
/// registered request transforms before the coordinator records it.
pub struct MiddlewareStepper {
    inner: Arc<dyn AgentStepper>,
    middleware: ModelMiddleware,
}

impl MiddlewareStepper {
    pub fn new(inner: Arc<dyn AgentStepper>, middleware: ModelMiddleware) -> Self {
        Self { inner, middleware }
    }
}

impl AgentStepper for MiddlewareStepper {
    fn initialize(&self, state: &mut AgentState, history: &[HistoryItem]) {
        self.inner.initialize(state, history);
    }

    fn pending_tool_calls(&self, state: &AgentState) -> Vec<ToolCallRequest> {
        self.inner.pending_tool_calls(state)
    }

    fn protocol_version(&self) -> u32 {
        self.inner.protocol_version()
    }

    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<Transition, StepError> {
        let mut transition = self.inner.transition(state, event)?;
        if self.middleware.is_empty() {
            return Ok(transition);
        }
        let ctx = ModelBuildContext {
            run_id: state.run_id.clone(),
            turn: transition.next.budget.turns_used,
        };
        for intent in &mut transition.intents {
            if let EffectIntent::CallModel(spec) = intent {
                let draft = ModelRequestDraft::from_spec(spec.clone());
                let draft = self
                    .middleware
                    .apply(&ctx, draft)
                    .map_err(|error| StepError::new(error.to_string()))?;
                *spec = draft.into_spec();
            }
        }
        Ok(transition)
    }
}

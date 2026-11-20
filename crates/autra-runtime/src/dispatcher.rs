//! Effect execution boundary. The Agent calls a Dispatcher instead of model and
//! tools directly; OutboxDispatcher is the claim-first path a durable host can
//! drive once effects are enqueued.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use autra_core::{
    AttemptStatus, EffectId, EffectIntent, EffectRecord, EffectStatus, ModelCallSpec, ModelOutput,
    RunId, ToolCallRequest, ToolOutcome,
};
use autra_harness::{ModelError, ModelProvider, ToolContext, ToolInput, ToolRegistry};
use futures_util::FutureExt;

use crate::journal::{EffectJournal, JournalError};

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

#[derive(Debug)]
pub enum DispatchOutcome {
    Model(Result<ModelOutput, ModelError>),
    Tool(ToolOutcome),
}

#[derive(Debug)]
pub struct DispatchResult {
    pub effect_id: EffectId,
    pub outcome: DispatchOutcome,
}

/// Claim-driven outbox loop. It executes the locked effect only after claiming
/// it, then records the outcome and completes the outbox row.
pub struct OutboxDispatcher;

impl OutboxDispatcher {
    pub async fn run_once_for_run(
        journal: &dyn EffectJournal,
        run_id: &RunId,
        limit: usize,
        dispatcher: &dyn Dispatcher,
        timeout: Option<Duration>,
    ) -> Result<Vec<DispatchResult>, JournalError> {
        let entries = journal.claim_due_effects_for_run(run_id, limit, "local-dispatcher")?;
        let mut results = Vec::new();
        for entry in entries {
            let intent: EffectIntent = match serde_json::from_str(&entry.intent_json) {
                Ok(intent) => intent,
                Err(error) => {
                    let _ = journal.release_outbox(&entry.effect_id);
                    return Err(JournalError::new(format!(
                        "cannot decode effect intent: {error}"
                    )));
                }
            };
            let outcome = match &intent {
                EffectIntent::CallModel(spec) => {
                    DispatchOutcome::Model(dispatcher.call_model(spec, timeout).await)
                }
                EffectIntent::CallTool(spec) => {
                    DispatchOutcome::Tool(dispatcher.call_tool(run_id, &spec.call, timeout).await)
                }
            };
            let mut effects = journal.load(run_id)?;
            let Some(record) = effects
                .iter_mut()
                .find(|record| record.id == entry.effect_id)
            else {
                let _ = journal.release_outbox(&entry.effect_id);
                return Err(JournalError::new(format!(
                    "effect record `{}` not found",
                    entry.effect_id
                )));
            };
            apply_outcome(record, &outcome);
            if let Err(error) = journal.record(run_id, record) {
                let _ = journal.release_outbox(&entry.effect_id);
                return Err(error);
            }
            journal.complete_outbox(&entry.effect_id)?;
            results.push(DispatchResult {
                effect_id: entry.effect_id,
                outcome,
            });
        }
        Ok(results)
    }
}

fn apply_outcome(record: &mut EffectRecord, outcome: &DispatchOutcome) {
    match outcome {
        DispatchOutcome::Model(Ok(output)) => {
            record.set_status(EffectStatus::Committed);
            record.record_attempt(AttemptStatus::Succeeded, output.text.clone(), None);
        }
        DispatchOutcome::Model(Err(error)) => {
            record.set_status(EffectStatus::Failed);
            record.record_attempt(AttemptStatus::Failed, None, Some(error.to_string()));
        }
        DispatchOutcome::Tool(outcome) if outcome.outcome_unknown => {
            record.set_status(EffectStatus::OutcomeUnknown);
            record.record_attempt(
                AttemptStatus::OutcomeUnknown,
                Some(outcome.content.clone()),
                None,
            );
        }
        DispatchOutcome::Tool(outcome) if outcome.is_error => {
            record.set_status(EffectStatus::Committed);
            record.record_attempt(
                AttemptStatus::Succeeded,
                Some(outcome.content.clone()),
                Some(outcome.content.clone()),
            );
        }
        DispatchOutcome::Tool(outcome) => {
            record.set_status(EffectStatus::Committed);
            record.record_attempt(
                AttemptStatus::Succeeded,
                Some(outcome.content.clone()),
                None,
            );
        }
    }
}

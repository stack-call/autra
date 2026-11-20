//! Effect execution boundary. The Agent calls a Dispatcher instead of model and
//! tools directly; OutboxDispatcher is the claim-first path a durable host can
//! drive once effects are enqueued.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use autra_core::{
    AttemptStatus, CancelFlag, EffectId, EffectIntent, EffectRecord, EffectStatus, ModelCallSpec,
    ModelOutput, RunId, ToolCallRequest, ToolOutcome,
};
use autra_execution::{ModelError, ModelProvider, ToolContext, ToolInput, ToolRegistry};
use futures_util::{FutureExt, StreamExt};

use crate::journal::{EffectJournal, JournalError, OutboxEntry};

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

    /// Whether the named tool can be executed here. A dispatcher that cannot
    /// answer keeps the default; the outbox loop uses this to tell an unknown
    /// tool apart from a tool that returned an error.
    fn knows_tool(&self, _name: &str) -> bool {
        true
    }
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

    fn knows_tool(&self, name: &str) -> bool {
        self.tools.get(name).is_some()
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
                Ok(Ok(Err(error))) => Err((error.to_string(), error.outcome_unknown)),
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

pub use autra_execution::journal::DEFAULT_CLAIM_LEASE;

#[derive(Debug)]
pub enum DispatchOutcome {
    Model(Result<ModelOutput, ModelError>),
    Tool(ToolOutcome),
    Cancelled,
    /// The effect record was already terminal; the stale outbox row was removed.
    AlreadyResolved,
    /// The record was in flight when its claim lease expired; it is now unknown.
    Interrupted,
}

#[derive(Debug)]
pub struct DispatchResult {
    pub effect_id: EffectId,
    pub outcome: DispatchOutcome,
}

/// Knobs for one outbox pass.
#[derive(Clone, Copy, Debug)]
pub struct DispatchOptions<'a> {
    pub limit: usize,
    pub max_parallel: usize,
    pub timeout: Option<Duration>,
    pub cancel: Option<&'a CancelFlag>,
    pub lease: Duration,
}

/// Claim-driven outbox loop. It executes only claimed effects, records the
/// outcome and completes the outbox row.
pub struct OutboxDispatcher;

impl OutboxDispatcher {
    /// Dispatch up to `limit` due effects for one Run, sequentially.
    pub async fn run_once_for_run(
        journal: &dyn EffectJournal,
        run_id: &RunId,
        limit: usize,
        dispatcher: &dyn Dispatcher,
        timeout: Option<Duration>,
    ) -> Result<Vec<DispatchResult>, JournalError> {
        Self::run_once_for_run_with(
            journal,
            run_id,
            DispatchOptions {
                limit,
                max_parallel: 1,
                timeout,
                cancel: None,
                lease: DEFAULT_CLAIM_LEASE,
            },
            dispatcher,
            |_| {},
        )
        .await
    }

    /// Claim due effects for one Run, dispatch them with bounded parallelism
    /// and report each finished effect through `on_result` in completion order.
    /// A cancellation observed before an effect starts leaves the effect
    /// `CancelledBeforeDispatch` and removes its outbox row.
    pub async fn run_once_for_run_with<F>(
        journal: &dyn EffectJournal,
        run_id: &RunId,
        options: DispatchOptions<'_>,
        dispatcher: &dyn Dispatcher,
        on_result: F,
    ) -> Result<Vec<DispatchResult>, JournalError>
    where
        F: FnMut(&DispatchResult),
    {
        let entries = journal.claim_due_effects_for_run_with_lease(
            run_id,
            options.limit,
            "local-dispatcher",
            options.lease,
        )?;
        dispatch_entries(journal, entries, options, dispatcher, on_result).await
    }

    /// Drain due outbox rows across all Runs. Rows whose lease expired are
    /// reclaimed; a record that was already terminal is only cleaned up, and an
    /// in-flight record becomes `OutcomeUnknown` instead of being retried.
    pub async fn run_due(
        journal: &dyn EffectJournal,
        limit: usize,
        dispatcher: &dyn Dispatcher,
        timeout: Option<Duration>,
    ) -> Result<Vec<DispatchResult>, JournalError> {
        Self::run_due_with(
            journal,
            DispatchOptions {
                limit,
                max_parallel: 1,
                timeout,
                cancel: None,
                lease: DEFAULT_CLAIM_LEASE,
            },
            dispatcher,
            |_| {},
        )
        .await
    }

    pub async fn run_due_with<F>(
        journal: &dyn EffectJournal,
        options: DispatchOptions<'_>,
        dispatcher: &dyn Dispatcher,
        on_result: F,
    ) -> Result<Vec<DispatchResult>, JournalError>
    where
        F: FnMut(&DispatchResult),
    {
        let entries =
            journal.claim_due_effects_with_lease(options.limit, "outbox-drain", options.lease)?;
        dispatch_entries(journal, entries, options, dispatcher, on_result).await
    }
}

async fn dispatch_entries<F>(
    journal: &dyn EffectJournal,
    entries: Vec<OutboxEntry>,
    options: DispatchOptions<'_>,
    dispatcher: &dyn Dispatcher,
    mut on_result: F,
) -> Result<Vec<DispatchResult>, JournalError>
where
    F: FnMut(&DispatchResult),
{
    let mut futures = Vec::with_capacity(entries.len());
    for entry in &entries {
        futures.push(dispatch_entry(
            journal,
            &entry.run_id,
            entry,
            dispatcher,
            options.timeout,
            options.cancel,
        ));
    }
    let mut stream =
        futures_util::stream::iter(futures).buffer_unordered(options.max_parallel.max(1));
    let mut results = Vec::with_capacity(entries.len());
    while let Some(result) = stream.next().await {
        let result = result?;
        on_result(&result);
        results.push(result);
    }
    Ok(results)
}

async fn dispatch_entry(
    journal: &dyn EffectJournal,
    run_id: &RunId,
    entry: &OutboxEntry,
    dispatcher: &dyn Dispatcher,
    timeout: Option<Duration>,
    cancel: Option<&CancelFlag>,
) -> Result<DispatchResult, JournalError> {
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
    match record.status {
        EffectStatus::Committed
        | EffectStatus::Failed
        | EffectStatus::Rejected
        | EffectStatus::OutcomeUnknown
        | EffectStatus::CancelledBeforeDispatch => {
            journal.complete_outbox(&entry.effect_id)?;
            return Ok(DispatchResult {
                effect_id: entry.effect_id.clone(),
                outcome: DispatchOutcome::AlreadyResolved,
            });
        }
        EffectStatus::Dispatched => {
            record.set_status(EffectStatus::OutcomeUnknown);
            record.record_attempt(
                AttemptStatus::OutcomeUnknown,
                None,
                Some("the effect was in flight when its claim lease ended".to_owned()),
            );
            journal.record(run_id, record)?;
            journal.complete_outbox(&entry.effect_id)?;
            return Ok(DispatchResult {
                effect_id: entry.effect_id.clone(),
                outcome: DispatchOutcome::Interrupted,
            });
        }
        _ => {}
    }

    if cancel.map(CancelFlag::is_cancelled).unwrap_or(false) {
        return finish_entry(journal, run_id, entry, DispatchOutcome::Cancelled, true);
    }
    let intent: EffectIntent = match serde_json::from_str(&entry.intent_json) {
        Ok(intent) => intent,
        Err(error) => {
            let _ = journal.release_outbox(&entry.effect_id);
            return Err(JournalError::new(format!(
                "cannot decode effect intent: {error}"
            )));
        }
    };
    let known_tool = match &intent {
        EffectIntent::CallModel(_) => true,
        EffectIntent::CallTool(spec) => dispatcher.knows_tool(&spec.call.name),
    };
    mark_dispatched(journal, run_id, entry)?;
    let outcome = match &intent {
        EffectIntent::CallModel(spec) => {
            DispatchOutcome::Model(dispatcher.call_model(spec, timeout).await)
        }
        EffectIntent::CallTool(spec) => {
            DispatchOutcome::Tool(dispatcher.call_tool(run_id, &spec.call, timeout).await)
        }
    };
    finish_entry(journal, run_id, entry, outcome, known_tool)
}

fn mark_dispatched(
    journal: &dyn EffectJournal,
    run_id: &RunId,
    entry: &OutboxEntry,
) -> Result<(), JournalError> {
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
    record.set_status(EffectStatus::Dispatched);
    if let Err(error) = journal.record(run_id, record) {
        let _ = journal.release_outbox(&entry.effect_id);
        return Err(error);
    }
    Ok(())
}

fn finish_entry(
    journal: &dyn EffectJournal,
    run_id: &RunId,
    entry: &OutboxEntry,
    outcome: DispatchOutcome,
    known_tool: bool,
) -> Result<DispatchResult, JournalError> {
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
    apply_outcome(record, &outcome, known_tool);
    if let Err(error) = journal.record(run_id, record) {
        let _ = journal.release_outbox(&entry.effect_id);
        return Err(error);
    }
    journal.complete_outbox(&entry.effect_id)?;
    Ok(DispatchResult {
        effect_id: entry.effect_id.clone(),
        outcome,
    })
}

fn apply_outcome(record: &mut EffectRecord, outcome: &DispatchOutcome, known_tool: bool) {
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
        DispatchOutcome::Tool(outcome) if !known_tool => {
            record.set_status(EffectStatus::Failed);
            record.record_attempt(AttemptStatus::Failed, None, Some(outcome.content.clone()));
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
        DispatchOutcome::Cancelled => {
            record.set_status(EffectStatus::CancelledBeforeDispatch);
        }
        DispatchOutcome::AlreadyResolved | DispatchOutcome::Interrupted => {}
    }
}

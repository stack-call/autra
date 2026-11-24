//! Effect execution boundary. The Agent calls a Dispatcher instead of model and
//! tools directly; OutboxDispatcher is the claim-first path a durable host can
//! drive once effects are enqueued.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use autra_core::{
    AttemptStatus, BudgetHandle, CancelFlag, EffectId, EffectIntent, EffectRecord, EffectStatus,
    ModelCallSpec, ModelOutput, RunId, ToolCallRequest, ToolOutcome,
};
use autra_execution::{
    ChildExecutionContext, ModelError, ModelProvider, ToolContext, ToolInput, ToolRegistry,
    ToolResource, emit_complete_text,
};
use futures_util::{FutureExt, StreamExt};

use crate::journal::{EffectJournal, JournalError, OutboxEntry};
use crate::scheduler::{AcquireError, GlobalScheduler, ResourceKind};

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

    /// Streaming model call. The default emits the complete text once.
    async fn call_model_streaming(
        &self,
        request: &ModelCallSpec,
        timeout: Option<Duration>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, ModelError> {
        let output = self.call_model(request, timeout).await?;
        emit_complete_text(&output, on_delta);
        Ok(output)
    }

    /// Execute a tool with the invoking scope. Custom dispatchers that only
    /// know `call_tool` keep working through this default.
    async fn call_tool_with_context(
        &self,
        context: &ToolContext,
        call: &ToolCallRequest,
        timeout: Option<Duration>,
    ) -> ToolOutcome {
        self.call_tool(&context.run_id, call, timeout).await
    }

    /// Whether the named tool can be executed here. A dispatcher that cannot
    /// answer keeps the default; the outbox loop uses this to tell an unknown
    /// tool apart from a tool that returned an error.
    fn knows_tool(&self, _name: &str) -> bool {
        true
    }

    /// The resource pool used by a tool. A dispatcher that cannot answer keeps
    /// the generic tool pool.
    fn tool_resource(&self, _name: &str) -> ToolResource {
        ToolResource::Tool
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
        let context = ToolContext {
            run_id: run_id.clone(),
            call_id: call.call_id.clone(),
            parent_run_id: Some(run_id.clone()),
            cancel: None,
            budget: None,
            child_execution: None,
        };
        execute_tool(self.tools.clone(), call.clone(), timeout, context).await
    }

    async fn call_tool_with_context(
        &self,
        context: &ToolContext,
        call: &ToolCallRequest,
        timeout: Option<Duration>,
    ) -> ToolOutcome {
        execute_tool(self.tools.clone(), call.clone(), timeout, context.clone()).await
    }

    async fn call_model_streaming(
        &self,
        request: &ModelCallSpec,
        timeout: Option<Duration>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ModelOutput, ModelError> {
        match with_timeout(timeout, self.model.complete_streaming(request, on_delta)).await {
            Ok(result) => result,
            Err(()) => Err(ModelError::new(
                "model call exceeded the configured timeout",
            )),
        }
    }

    fn knows_tool(&self, name: &str) -> bool {
        self.tools.get(name).is_some()
    }

    fn tool_resource(&self, name: &str) -> ToolResource {
        self.tools
            .get(name)
            .map(|tool| tool.resource())
            .unwrap_or_default()
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
    context: ToolContext,
) -> ToolOutcome {
    let result: Result<String, (String, bool)> = match tools.get(&call.name) {
        Some(tool) => {
            let input = ToolInput {
                arguments: call.arguments.clone(),
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

/// Receives streamed model text as it arrives. A sink is shared so a caller can
/// keep one consumer while several claimed effects run concurrently.
pub type DeltaSink = Arc<dyn Fn(String) + Send + Sync>;

/// Knobs for one outbox pass.
#[derive(Clone)]
pub struct DispatchOptions<'a> {
    pub limit: usize,
    pub max_parallel: usize,
    pub timeout: Option<Duration>,
    pub cancel: Option<&'a CancelFlag>,
    pub lease: Duration,
    pub budget: Option<BudgetHandle>,
    pub child_execution: Option<ChildExecutionContext>,
    /// When set, model effects stream their text through this sink.
    pub delta: Option<DeltaSink>,
    /// Global resource pools. `None` keeps the historical unbounded behavior.
    pub scheduler: Option<Arc<GlobalScheduler>>,
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
                budget: None,
                child_execution: None,
                delta: None,
                scheduler: None,
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
        let claim_id = next_claim_id("local-dispatcher");
        let lease = options.lease;
        let entries = journal.claim_due_effects_for_run_with_lease(
            run_id,
            options.limit,
            &claim_id,
            lease,
        )?;
        dispatch_entries(
            journal, entries, options, dispatcher, &claim_id, lease, on_result,
        )
        .await
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
                budget: None,
                child_execution: None,
                delta: None,
                scheduler: None,
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
        let claim_id = next_claim_id("outbox-drain");
        let lease = options.lease;
        let entries = journal.claim_due_effects_with_lease(options.limit, &claim_id, lease)?;
        dispatch_entries(
            journal, entries, options, dispatcher, &claim_id, lease, on_result,
        )
        .await
    }
}

pub(crate) async fn await_with_cancel<F, T>(future: F, cancel: Option<&CancelFlag>) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    let Some(cancel) = cancel else {
        return Some(future.await);
    };
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Some(result),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if cancel.is_cancelled() {
                    return None;
                }
            }
        }
    }
}

/// Keep a claimed Effect alive while its future runs. A lost or expired claim
/// returns an error so the dispatcher never records an outcome it no longer
/// owns.
async fn await_with_lease<F, T>(
    journal: &dyn EffectJournal,
    effect_id: &EffectId,
    claim_id: &str,
    lease: Duration,
    future: F,
) -> Result<T, JournalError>
where
    F: Future<Output = T>,
{
    tokio::pin!(future);
    let period = (lease / 3).max(Duration::from_millis(100));
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = ticker.tick() => {
                if !journal.renew_claim(effect_id, claim_id, lease)? {
                    return Err(JournalError::claim_lost(effect_id, claim_id));
                }
            }
        }
    }
}

fn next_claim_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

async fn dispatch_entries<F>(
    journal: &dyn EffectJournal,
    entries: Vec<OutboxEntry>,
    options: DispatchOptions<'_>,
    dispatcher: &dyn Dispatcher,
    claim_id: &str,
    lease: Duration,
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
            &options,
            claim_id,
            lease,
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
    options: &DispatchOptions<'_>,
    claim_id: &str,
    lease: Duration,
) -> Result<DispatchResult, JournalError> {
    let mut effects = journal.load(run_id)?;
    let Some(record) = effects
        .iter_mut()
        .find(|record| record.id == entry.effect_id)
    else {
        let _ = journal.release_outbox(&entry.effect_id, claim_id);
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
            journal.complete_outbox(&entry.effect_id, claim_id)?;
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
            stamp_attempt(record, claim_id, entry.attempts);
            journal.record_claimed(run_id, record, claim_id)?;
            journal.complete_outbox(&entry.effect_id, claim_id)?;
            return Ok(DispatchResult {
                effect_id: entry.effect_id.clone(),
                outcome: DispatchOutcome::Interrupted,
            });
        }
        _ => {}
    }

    if options
        .cancel
        .map(CancelFlag::is_cancelled)
        .unwrap_or(false)
    {
        return finish_entry(
            journal,
            run_id,
            entry,
            DispatchOutcome::Cancelled,
            true,
            claim_id,
        );
    }
    let intent: EffectIntent = match serde_json::from_str(&entry.intent_json) {
        Ok(intent) => intent,
        Err(error) => {
            let _ = journal.release_outbox(&entry.effect_id, claim_id);
            return Err(JournalError::new(format!(
                "cannot decode effect intent: {error}"
            )));
        }
    };
    let known_tool = match &intent {
        EffectIntent::CallModel(_) => true,
        EffectIntent::CallTool(spec) => dispatcher.knows_tool(&spec.call.name),
    };
    let resource = match &intent {
        EffectIntent::CallModel(_) => ResourceKind::Model,
        EffectIntent::CallTool(spec) => match dispatcher.tool_resource(&spec.call.name) {
            ToolResource::Process => ResourceKind::Process,
            ToolResource::Tool => ResourceKind::Tool,
        },
    };
    let permit = match options.scheduler.clone() {
        Some(scheduler) => {
            let acquire = scheduler.acquire(resource, run_id, options.cancel);
            match await_with_lease(journal, &entry.effect_id, claim_id, lease, acquire).await {
                Ok(Ok(permit)) => Some(permit),
                Ok(Err(AcquireError::Cancelled)) => {
                    return finish_entry(
                        journal,
                        run_id,
                        entry,
                        DispatchOutcome::Cancelled,
                        known_tool,
                        claim_id,
                    );
                }
                Ok(Err(AcquireError::QueueFull)) => {
                    return Err(JournalError::new("scheduler queue is full"));
                }
                Err(error) => return Err(error),
            }
        }
        None => None,
    };
    mark_dispatched(journal, run_id, entry, claim_id)?;
    let call = async {
        match &intent {
            EffectIntent::CallModel(spec) => {
                let result = match options.delta.clone() {
                    Some(sink) => {
                        let mut on_delta = move |text: String| sink(text);
                        let call =
                            dispatcher.call_model_streaming(spec, options.timeout, &mut on_delta);
                        await_with_cancel(call, options.cancel).await
                    }
                    None => {
                        let call = dispatcher.call_model(spec, options.timeout);
                        await_with_cancel(call, options.cancel).await
                    }
                };
                match result {
                    Some(result) => DispatchOutcome::Model(result),
                    None => DispatchOutcome::Cancelled,
                }
            }
            EffectIntent::CallTool(spec) => {
                let context = ToolContext {
                    run_id: run_id.clone(),
                    call_id: spec.call.call_id.clone(),
                    parent_run_id: Some(run_id.clone()),
                    cancel: options.cancel.cloned(),
                    budget: options.budget.clone(),
                    child_execution: options.child_execution.clone(),
                };
                DispatchOutcome::Tool(
                    dispatcher
                        .call_tool_with_context(&context, &spec.call, options.timeout)
                        .await,
                )
            }
        }
    };
    let outcome = match await_with_lease(journal, &entry.effect_id, claim_id, lease, call).await {
        Ok(outcome) => outcome,
        Err(error) => {
            if !error.is_claim_lost() {
                let _ = journal.release_outbox(&entry.effect_id, claim_id);
            }
            return Err(error);
        }
    };
    drop(permit);
    finish_entry(journal, run_id, entry, outcome, known_tool, claim_id)
}

fn mark_dispatched(
    journal: &dyn EffectJournal,
    run_id: &RunId,
    entry: &OutboxEntry,
    claim_id: &str,
) -> Result<(), JournalError> {
    let mut effects = journal.load(run_id)?;
    let Some(record) = effects
        .iter_mut()
        .find(|record| record.id == entry.effect_id)
    else {
        let _ = journal.release_outbox(&entry.effect_id, claim_id);
        return Err(JournalError::new(format!(
            "effect record `{}` not found",
            entry.effect_id
        )));
    };
    record.set_status(EffectStatus::Dispatched);
    if let Err(error) = journal.record_claimed(run_id, record, claim_id) {
        if !error.is_claim_lost() {
            let _ = journal.release_outbox(&entry.effect_id, claim_id);
        }
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
    claim_id: &str,
) -> Result<DispatchResult, JournalError> {
    let mut effects = journal.load(run_id)?;
    let Some(record) = effects
        .iter_mut()
        .find(|record| record.id == entry.effect_id)
    else {
        let _ = journal.release_outbox(&entry.effect_id, claim_id);
        return Err(JournalError::new(format!(
            "effect record `{}` not found",
            entry.effect_id
        )));
    };
    apply_outcome(record, &outcome, known_tool);
    stamp_attempt(record, claim_id, entry.attempts);
    if let Err(error) = journal.record_claimed(run_id, record, claim_id) {
        if !error.is_claim_lost() {
            let _ = journal.release_outbox(&entry.effect_id, claim_id);
        }
        return Err(error);
    }
    journal.complete_outbox(&entry.effect_id, claim_id)?;
    Ok(DispatchResult {
        effect_id: entry.effect_id.clone(),
        outcome,
    })
}

fn stamp_attempt(record: &mut EffectRecord, claim_id: &str, fencing_token: u32) {
    if let Some(attempt) = record.attempts.last_mut() {
        attempt.worker_id = Some(claim_id.to_owned());
        attempt.fencing_token = Some(fencing_token as u64);
    }
}

fn apply_outcome(record: &mut EffectRecord, outcome: &DispatchOutcome, known_tool: bool) {
    match outcome {
        DispatchOutcome::Model(Ok(output)) => record.apply_model_success(output.text.clone()),
        DispatchOutcome::Model(Err(error)) => record.apply_model_failure(error.to_string()),
        DispatchOutcome::Tool(outcome) => record.apply_tool_outcome(outcome, known_tool),
        DispatchOutcome::Cancelled => {
            record.set_status(EffectStatus::CancelledBeforeDispatch);
        }
        DispatchOutcome::AlreadyResolved | DispatchOutcome::Interrupted => {}
    }
}

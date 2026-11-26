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
use crate::scheduler::{AcquireError, EffectPermit, GlobalScheduler, ResourceKind, ScheduleError};

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

    fn tool_environment(
        &self,
        _name: &str,
    ) -> Option<Arc<dyn autra_execution::ExecutionEnvironment>> {
        None
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
            environment: None,
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

    fn tool_environment(
        &self,
        name: &str,
    ) -> Option<Arc<dyn autra_execution::ExecutionEnvironment>> {
        self.tools.get(name).and_then(|t| t.environment())
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
    /// Publish full results atomically for a message-driven Coordinator.
    pub result_inbox: bool,
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
                result_inbox: false,
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
        if options.result_inbox && !journal.supports_result_inbox() {
            return Err(JournalError::new(
                "journal does not support an atomic result inbox",
            ));
        }
        if options.scheduler.is_some() && journal.supports_result_inbox() {
            return dispatch_scheduled(journal, Some(run_id), options, dispatcher, on_result).await;
        }
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
                result_inbox: false,
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
        if options.result_inbox && !journal.supports_result_inbox() {
            return Err(JournalError::new(
                "journal does not support an atomic result inbox",
            ));
        }
        if options.scheduler.is_some() && journal.supports_result_inbox() {
            return dispatch_scheduled(journal, None, options, dispatcher, on_result).await;
        }
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
    crate::engine::lease::next_owner_id(prefix)
}

async fn dispatch_entries<F>(
    journal: &dyn EffectJournal,
    entries: Vec<OutboxEntry>,
    options: DispatchOptions<'_>,
    dispatcher: &dyn Dispatcher,
    claim: &str,
    lease: Duration,
    mut on_result: F,
) -> Result<Vec<DispatchResult>, JournalError>
where
    F: FnMut(&DispatchResult),
{
    let mut futures = Vec::with_capacity(entries.len());
    for e in &entries {
        futures.push(dispatch_entry(
            journal, e, dispatcher, &options, claim, lease, None,
        ));
    }
    let mut pending =
        futures_util::stream::iter(futures).buffer_unordered(options.max_parallel.max(1));
    let mut results = Vec::new();
    while let Some(r) = pending.next().await {
        let r = r?;
        on_result(&r);
        results.push(r);
    }
    Ok(results)
}
async fn dispatch_entry(
    journal: &dyn EffectJournal,
    entry: &OutboxEntry,
    dispatcher: &dyn Dispatcher,
    options: &DispatchOptions<'_>,
    claim: &str,
    lease: Duration,
    reservation: Option<EffectPermit>,
) -> Result<DispatchResult, JournalError> {
    let run = &entry.run_id;
    let Some(mut record) = journal.load_effect(run, &entry.effect_id)? else {
        let _ = journal.release_outbox(&entry.effect_id, claim);
        return Err(JournalError::new("effect record is missing"));
    };
    if record.result_inbox && !journal.supports_result_inbox() {
        return Err(JournalError::new(
            "journal does not support this Effect's result inbox",
        ));
    }
    match record.status {
        EffectStatus::Committed
        | EffectStatus::Failed
        | EffectStatus::Rejected
        | EffectStatus::OutcomeUnknown
        | EffectStatus::CancelledBeforeDispatch => {
            if options.result_inbox || record.result_inbox {
                publish_result(
                    journal,
                    entry,
                    &record,
                    claim,
                    outcome_from_record(&record),
                    options.budget.as_ref(),
                )?;
            } else {
                journal.complete_outbox(&entry.effect_id, claim)?;
            }
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
                Some("the effect was in flight when its claim lease ended".into()),
            );
            stamp_attempt(&mut record, claim, entry.attempts);
            if options.result_inbox || record.result_inbox {
                publish_result(
                    journal,
                    entry,
                    &record,
                    claim,
                    outcome_from_record(&record),
                    options.budget.as_ref(),
                )?;
            } else {
                journal.record_claimed(run, &record, claim)?;
                journal.complete_outbox(&entry.effect_id, claim)?;
            }
            return Ok(DispatchResult {
                effect_id: entry.effect_id.clone(),
                outcome: DispatchOutcome::Interrupted,
            });
        }
        EffectStatus::Authorized => {}
        _ => {
            let _ = journal.release_outbox(&entry.effect_id, claim);
            return Err(JournalError::new("effect is not authorized for dispatch"));
        }
    }
    if options.cancel.is_some_and(CancelFlag::is_cancelled) {
        return finish_entry(
            journal,
            entry,
            DispatchOutcome::Cancelled,
            true,
            claim,
            options,
        );
    }
    let intent: EffectIntent = serde_json::from_str(&entry.intent_json)
        .map_err(|e| JournalError::new(format!("cannot decode effect intent: {e}")))?;
    let (known, resource, fallback) = match &intent {
        EffectIntent::CallModel(_) => (true, ResourceKind::Model, None),
        EffectIntent::CallTool(spec) => (
            dispatcher.knows_tool(&spec.call.name),
            match dispatcher.tool_resource(&spec.call.name) {
                ToolResource::Process => ResourceKind::Process,
                ToolResource::Tool => ResourceKind::Tool,
            },
            dispatcher.tool_environment(&spec.call.name),
        ),
    };
    let environment = reservation.as_ref().and_then(|p| p.environment.clone());
    let _reservation = reservation;
    let permit = match options.scheduler.clone().filter(|_| _reservation.is_none()) {
        Some(scheduler) => {
            if fallback.is_some() && record.requirements.environment.is_none() {
                return Err(JournalError::new(
                    "recorded effect has no environment identity; refusing to infer a workspace during recovery",
                ));
            }
            let acquire = scheduler.acquire_effect(
                resource,
                run,
                options.cancel,
                &record.requirements,
                fallback,
            );
            match await_with_lease(journal, &entry.effect_id, claim, lease, acquire).await? {
                Ok(p) => Some(p),
                Err(ScheduleError::Resource(AcquireError::Cancelled)) => {
                    return finish_entry(
                        journal,
                        entry,
                        DispatchOutcome::Cancelled,
                        known,
                        claim,
                        options,
                    );
                }
                Err(e) => return Err(JournalError::new(e.to_string())),
            }
        }
        None => None,
    };
    let environment = environment.or_else(|| permit.as_ref().and_then(|p| p.environment.clone()));
    if record.requirements.environment.is_some() && environment.is_none() {
        let _ = journal.release_outbox(&entry.effect_id, claim);
        return Err(JournalError::new(
            "environment requirements need an environment-aware scheduler",
        ));
    }
    mark_dispatched(journal, entry, claim, environment.as_ref())?;
    if let (Some(budget), Some(state)) = (&options.budget, journal.load_run(run)?) {
        if !budget.advance_to(&state.budget) {
            return Err(JournalError::new(
                "execution budget limits do not match the Run",
            ));
        }
    }
    let call = async {
        match &intent {
            EffectIntent::CallModel(spec) => {
                let output = match options.delta.clone() {
                    Some(sink) => {
                        let mut emit = move |text: String| sink(text);
                        await_with_cancel(
                            dispatcher.call_model_streaming(spec, options.timeout, &mut emit),
                            options.cancel,
                        )
                        .await
                    }
                    None => {
                        await_with_cancel(
                            dispatcher.call_model(spec, options.timeout),
                            options.cancel,
                        )
                        .await
                    }
                };
                match output {
                    Some(output) => DispatchOutcome::Model(output),
                    None => DispatchOutcome::Cancelled,
                }
            }
            EffectIntent::CallTool(spec) => {
                let context = ToolContext {
                    environment: environment.clone(),
                    run_id: run.clone(),
                    call_id: spec.call.call_id.clone(),
                    parent_run_id: Some(run.clone()),
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
    let outcome = match await_with_lease(journal, &entry.effect_id, claim, lease, call).await {
        Ok(outcome) => outcome,
        Err(e) => {
            if !e.is_claim_lost() {
                let _ = journal.release_outbox(&entry.effect_id, claim);
            }
            return Err(e);
        }
    };
    drop(permit);
    finish_entry(journal, entry, outcome, known, claim, options)
}
fn mark_dispatched(
    journal: &dyn EffectJournal,
    entry: &OutboxEntry,
    claim: &str,
    environment: Option<&Arc<dyn autra_execution::ExecutionEnvironment>>,
) -> Result<(), JournalError> {
    let mut r = journal
        .load_effect(&entry.run_id, &entry.effect_id)?
        .ok_or_else(|| JournalError::new("effect record is missing"))?;
    if let Some(env) = environment {
        r.requirements.environment = Some(autra_core::EnvironmentAffinity::Required(
            env.id().to_owned(),
        ));
    }
    r.set_status(EffectStatus::Dispatched);
    journal.record_claimed(&entry.run_id, &r, claim)
}
fn finish_entry(
    journal: &dyn EffectJournal,
    entry: &OutboxEntry,
    outcome: DispatchOutcome,
    known: bool,
    claim: &str,
    options: &DispatchOptions<'_>,
) -> Result<DispatchResult, JournalError> {
    let mut record = journal
        .load_effect(&entry.run_id, &entry.effect_id)?
        .ok_or_else(|| JournalError::new("effect record is missing"))?;
    match &outcome {
        DispatchOutcome::Model(Ok(o)) => record.apply_model_success(o.text.clone()),
        DispatchOutcome::Model(Err(e)) => record.apply_model_failure(e.to_string()),
        DispatchOutcome::Tool(o) => record.apply_tool_outcome(o, known),
        DispatchOutcome::Cancelled => {
            if record.status == EffectStatus::Dispatched {
                record.set_status(EffectStatus::OutcomeUnknown);
                record.record_attempt(
                    AttemptStatus::OutcomeUnknown,
                    None,
                    Some("cancelled after dispatch; outcome unknown".into()),
                );
            } else {
                record.set_status(EffectStatus::CancelledBeforeDispatch);
            }
        }
        DispatchOutcome::AlreadyResolved | DispatchOutcome::Interrupted => {}
    }
    stamp_attempt(&mut record, claim, entry.attempts);
    if options.result_inbox || record.result_inbox {
        let result = match &outcome {
            DispatchOutcome::Model(Ok(o)) => autra_execution::EffectOutcome::Model(o.clone()),
            DispatchOutcome::Model(Err(e)) => {
                autra_execution::EffectOutcome::ModelFailed(e.to_string())
            }
            DispatchOutcome::Tool(o) => autra_execution::EffectOutcome::Tool(o.clone()),
            DispatchOutcome::Cancelled => autra_execution::EffectOutcome::Cancelled,
            _ => outcome_from_record(&record),
        };
        publish_result(
            journal,
            entry,
            &record,
            claim,
            result,
            options.budget.as_ref(),
        )?;
    } else {
        journal.record_claimed(&entry.run_id, &record, claim)?;
        journal.complete_outbox(&entry.effect_id, claim)?;
    }
    Ok(DispatchResult {
        effect_id: entry.effect_id.clone(),
        outcome,
    })
}
fn stamp_attempt(r: &mut EffectRecord, claim: &str, generation: u32) {
    if let Some(a) = r.attempts.last_mut() {
        a.worker_id = Some(claim.into());
        a.fencing_token = Some(generation as u64);
    }
}
fn outcome_from_record(r: &EffectRecord) -> autra_execution::EffectOutcome {
    if r.status == EffectStatus::CancelledBeforeDispatch {
        return autra_execution::EffectOutcome::Cancelled;
    }
    if let Some(o) = crate::engine::tool_outcome_from_record(r) {
        return autra_execution::EffectOutcome::Tool(o);
    }
    autra_execution::EffectOutcome::ModelFailed(r.error.clone().unwrap_or_else(|| {
        "full model result is unavailable; explicit reconciliation is required".into()
    }))
}
fn publish_result(
    journal: &dyn EffectJournal,
    entry: &OutboxEntry,
    r: &EffectRecord,
    claim: &str,
    outcome: autra_execution::EffectOutcome,
    budget: Option<&BudgetHandle>,
) -> Result<(), JournalError> {
    let attempt_id = if r.status == EffectStatus::CancelledBeforeDispatch {
        claim.to_owned()
    } else {
        r.attempts
            .last()
            .and_then(|a| a.id.as_ref())
            .map(|id| id.as_str().to_owned())
            .unwrap_or_else(|| claim.to_owned())
    };
    journal.record_result_claimed(
        &entry.run_id,
        r,
        claim,
        &autra_execution::EffectResult {
            identity: autra_execution::ResultIdentity {
                effect_id: r.id.clone(),
                attempt_id,
            },
            outcome,
            budget: budget.map(BudgetHandle::snapshot),
        },
    )
}
async fn dispatch_scheduled<F>(
    journal: &dyn EffectJournal,
    run: Option<&RunId>,
    options: DispatchOptions<'_>,
    dispatcher: &dyn Dispatcher,
    mut on_result: F,
) -> Result<Vec<DispatchResult>, JournalError>
where
    F: FnMut(&DispatchResult),
{
    let scheduler = options
        .scheduler
        .as_ref()
        .ok_or_else(|| JournalError::new("message-driven dispatch requires a scheduler"))?;
    let entries = journal.peek_due_effects(run, options.limit)?;
    let claim = next_claim_id("effect-worker");
    let mut futures = Vec::with_capacity(entries.len());
    for entry in &entries {
        futures.push(reserve_and_dispatch(
            journal, entry, &options, dispatcher, scheduler, &claim,
        ));
    }
    let mut pending =
        futures_util::stream::iter(futures).buffer_unordered(options.max_parallel.max(1));
    let mut results = Vec::new();
    while let Some(r) = pending.next().await {
        if let Some(r) = r? {
            on_result(&r);
            results.push(r);
        }
    }
    Ok(results)
}
async fn reserve_and_dispatch(
    journal: &dyn EffectJournal,
    candidate: &OutboxEntry,
    options: &DispatchOptions<'_>,
    dispatcher: &dyn Dispatcher,
    scheduler: &GlobalScheduler,
    claim: &str,
) -> Result<Option<DispatchResult>, JournalError> {
    let r = journal
        .load_effect(&candidate.run_id, &candidate.effect_id)?
        .ok_or_else(|| JournalError::new("scheduled effect is missing"))?;
    // These paths perform no resource I/O: an unavailable Environment must not
    // block cancellation or recording an interrupted attempt as unknown.
    if options.cancel.is_some_and(CancelFlag::is_cancelled) || r.status != EffectStatus::Authorized
    {
        let Some(entry) = journal.claim_effect(&candidate.effect_id, claim, options.lease)? else {
            return Ok(None);
        };
        return dispatch_entry(
            journal,
            &entry,
            dispatcher,
            options,
            claim,
            options.lease,
            None,
        )
        .await
        .map(Some);
    }
    let intent: EffectIntent = serde_json::from_str(&candidate.intent_json)
        .map_err(|e| JournalError::new(e.to_string()))?;
    let (resource, environment) = match &intent {
        EffectIntent::CallModel(_) => (ResourceKind::Model, None),
        EffectIntent::CallTool(spec) => (
            match dispatcher.tool_resource(&spec.call.name) {
                ToolResource::Process => ResourceKind::Process,
                ToolResource::Tool => ResourceKind::Tool,
            },
            dispatcher.tool_environment(&spec.call.name),
        ),
    };
    if environment.is_some() && r.requirements.environment.is_none() {
        return Err(JournalError::new(
            "recorded effect has no environment identity; refusing to infer a workspace during recovery",
        ));
    }
    let reservation = match scheduler
        .acquire_effect(
            resource,
            &candidate.run_id,
            options.cancel,
            &r.requirements,
            environment,
        )
        .await
    {
        Ok(p) => Some(p),
        Err(ScheduleError::Resource(AcquireError::Cancelled)) => None,
        Err(e) => return Err(JournalError::new(e.to_string())),
    };
    let Some(entry) = journal.claim_effect(&candidate.effect_id, claim, options.lease)? else {
        return Ok(None);
    };
    dispatch_entry(
        journal,
        &entry,
        dispatcher,
        options,
        claim,
        options.lease,
        reservation,
    )
    .await
    .map(Some)
}

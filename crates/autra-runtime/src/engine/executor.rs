//! Tool execution control: policy, approval, bounded concurrency, timeouts and
//! result recording. The run loop owns state transitions; this module only
//! executes Effects that were already committed.

use std::sync::Arc;
use std::time::Duration;

use autra_core::{
    BudgetHandle, CancelFlag, EffectId, EffectRecord, EffectStatus, ModelCallSpec, ModelOutput,
    RunId, ToolCallRequest, ToolOutcome,
};
use autra_execution::{
    Approver, ChildExecutionContext, DurableTransitionStore, ObserverHook, RunError, RunEvent,
};
use tracing::Instrument;

use super::streaming::{await_with_deltas, delta_channel};
use crate::dispatcher::{
    DEFAULT_CLAIM_LEASE, DispatchOptions, DispatchOutcome, DispatchResult, OutboxDispatcher,
    await_with_cancel,
};
use crate::scheduler::{AcquireError, ResourceKind, ScheduleError};

use super::coordinator::authorization::AuthorizationGate;
use super::coordinator::emit_tool_result;
use crate::dispatcher::Dispatcher;
use crate::scheduler::GlobalScheduler;
use autra_execution::{PolicyChain, ToolRegistry};

pub(super) enum ModelStep {
    Output(ModelOutput),
    Cancelled,
}

pub(super) struct ModelExecutionContext<'a, F> {
    pub on_event: &'a mut F,
    pub run_id: &'a RunId,
    pub effects: &'a mut Vec<EffectRecord>,
    pub observer: Option<&'a Arc<dyn ObserverHook>>,
    pub cancel: &'a CancelFlag,
    pub effect_timeout: Option<Duration>,
    pub turn: u32,
}

pub(super) struct ToolBatchContext<'a, F> {
    pub on_event: &'a mut F,
    pub run_id: &'a RunId,
    pub approver: Option<&'a Arc<dyn Approver>>,
    pub effects: &'a mut Vec<EffectRecord>,
    pub budget: &'a BudgetHandle,
    pub observer: Option<&'a Arc<dyn ObserverHook>>,
}

impl<'a, F> ToolBatchContext<'a, F>
where
    F: FnMut(&RunEvent),
{
    fn emit_stream(&mut self, event: &RunEvent) {
        (self.on_event)(event);
    }

    fn start_tool(&mut self, call: &ToolCallRequest) {
        self.emit_stream(&RunEvent::ToolCallStarted {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
        });
    }

    fn emit_tool_result(&mut self, outcome: &ToolOutcome) {
        emit_tool_result(&mut *self.on_event, outcome);
    }

    fn start_tool_outcome(&mut self, outcome: &ToolOutcome) {
        self.emit_stream(&RunEvent::ToolCallStarted {
            call_id: outcome.call_id.clone(),
            name: outcome.name.clone(),
        });
    }

    fn effect_mut(&mut self, effect_id: &EffectId) -> &mut EffectRecord {
        self.effects
            .iter_mut()
            .find(|record| &record.id == effect_id)
            .expect("effect record must exist")
    }

    fn record_tool_outcome(
        &mut self,
        effect_id: &EffectId,
        known_tool: bool,
        outcome: &ToolOutcome,
    ) {
        self.effect_mut(effect_id)
            .apply_tool_outcome(outcome, known_tool);
    }

    fn emit_authorized(&self, effect_id: &EffectId) {
        if let Some(observer) = self.observer {
            if let Some(record) = self.effects.iter().find(|record| &record.id == effect_id) {
                observer.on_effect_authorized(self.run_id, record);
            }
        }
    }

    fn emit_rejected(&self, effect_id: &EffectId) {
        if let Some(observer) = self.observer {
            if let Some(record) = self.effects.iter().find(|record| &record.id == effect_id) {
                observer.on_effect_rejected(self.run_id, record);
            }
        }
    }

    fn emit_resolved(&self, effect_id: &EffectId) {
        if let Some(observer) = self.observer {
            if let Some(record) = self.effects.iter().find(|record| &record.id == effect_id) {
                observer.on_effect_resolved(self.run_id, record);
            }
        }
    }
}

pub(super) struct ToolBatchOutcome {
    pub outcomes: Vec<ToolOutcome>,
    pub cancelled: bool,
}

/// Execution facilities, without RunState or a borrowed engine.
#[derive(Clone)]
pub struct ExecutionDependencies {
    pub dispatcher: Arc<dyn Dispatcher>,
    pub tools: ToolRegistry,
    pub journal: Option<Arc<dyn DurableTransitionStore>>,
    pub scheduler: Arc<GlobalScheduler>,
    pub approver: Option<Arc<dyn Approver>>,
    pub policy: PolicyChain,
    pub actor: Option<String>,
    pub approval_ttl: Option<Duration>,
}
/// Executes already committed Effects and never advances authoritative RunState.
#[derive(Clone)]
pub struct EffectExecutor {
    dependencies: ExecutionDependencies,
    authorization: AuthorizationGate,
}
impl EffectExecutor {
    pub fn new(dependencies: ExecutionDependencies) -> Self {
        let authorization = AuthorizationGate {
            tools: dependencies.tools.clone(),
            journal: dependencies.journal.clone(),
            policy: dependencies.policy.clone(),
            actor: dependencies.actor.clone(),
            approval_ttl: dependencies.approval_ttl,
            owner_id: None,
        };
        Self {
            dependencies,
            authorization,
        }
    }
    pub(super) fn with_owner(mut self, owner: Option<String>) -> Self {
        self.authorization = self.authorization.with_owner(owner);
        self
    }
    /// Can be hosted by a task independent of LocalEngine or its coordinator.
    pub async fn execute_pending(
        &self,
        run: &RunId,
        options: DispatchOptions<'_>,
    ) -> Result<Vec<DispatchResult>, autra_execution::JournalError> {
        let journal = self.dependencies.journal.as_ref().ok_or_else(|| {
            autra_execution::JournalError::new("independent execution requires a journal")
        })?;
        if options.result_inbox && !journal.supports_result_inbox() {
            return Err(autra_execution::JournalError::new(
                "journal does not support an atomic result inbox",
            ));
        }
        let cancel = options.cancel.cloned().unwrap_or_default();
        if journal.cancel_request(run)?.is_some() {
            cancel.cancel();
        }
        let options = DispatchOptions {
            cancel: Some(&cancel),
            scheduler: Some(self.dependencies.scheduler.clone()),
            ..options
        };
        let dispatch = OutboxDispatcher::run_once_for_run_with(
            journal.journal(),
            run,
            options,
            self.dependencies.dispatcher.as_ref(),
            |_| {},
        );
        tokio::pin!(dispatch);
        loop {
            tokio::select! {result=&mut dispatch=>return result,_=tokio::time::sleep(Duration::from_millis(100))=>{if journal.cancel_request(run)?.is_some(){cancel.cancel();}}}
        }
    }

    pub(super) async fn execute_model<F>(
        &self,
        effect_id: &EffectId,
        request: &ModelCallSpec,
        ctx: &mut ModelExecutionContext<'_, F>,
    ) -> Result<ModelStep, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let on_event = &mut *ctx.on_event;
        let run_id = ctx.run_id;
        let effects = &mut *ctx.effects;
        let observer = ctx.observer;
        let cancel = ctx.cancel;
        let effect_timeout = ctx.effect_timeout;
        let turn = ctx.turn;

        if cancel.is_cancelled() {
            let record = super::effect_mut(effects, effect_id);
            record.set_status(EffectStatus::CancelledBeforeDispatch);
            if let Some(journal) = &self.dependencies.journal {
                journal
                    .record(run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                journal
                    .complete_outbox(effect_id, autra_execution::UNCLAIMED_CLAIM)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
            return Ok(ModelStep::Cancelled);
        }

        super::effect_mut(effects, effect_id).set_status(EffectStatus::Authorized);
        on_event(&RunEvent::ModelCallStarted { turn });
        if let Some(observer) = observer {
            observer.on_model_request(run_id, turn);
        }
        let model_span = tracing::info_span!("gen_ai.model.call", turn);
        let output_result = async {
            let (delta, mut deltas) = delta_channel();
            match &self.dependencies.journal {
                Some(journal) => {
                    let options = DispatchOptions {
                        limit: 1,
                        max_parallel: 1,
                        timeout: effect_timeout,
                        cancel: Some(cancel),
                        lease: DEFAULT_CLAIM_LEASE,
                        budget: None,
                        child_execution: None,
                        delta: Some(delta),
                        scheduler: Some(self.dependencies.scheduler.clone()),
                        result_inbox: false,
                    };
                    let dispatch = OutboxDispatcher::run_once_for_run_with(
                        journal.as_ref(),
                        run_id,
                        options,
                        self.dependencies.dispatcher.as_ref(),
                        |_| {},
                    );
                    let results = await_with_deltas(dispatch, &mut deltas, on_event)
                        .await
                        .map_err(|error| RunError::Journal(error.to_string()))?;
                    super::sync_effect(journal.as_ref(), run_id, effect_id, effects)?;
                    let result = results
                        .into_iter()
                        .find(|result| result.effect_id == *effect_id)
                        .ok_or_else(|| {
                            RunError::Protocol(
                                "the claimed model effect produced no outcome".to_owned(),
                            )
                        })?;
                    match result.outcome {
                        DispatchOutcome::Model(output) => {
                            output.map(ModelStep::Output).map_err(RunError::Model)
                        }
                        DispatchOutcome::Cancelled => Ok(ModelStep::Cancelled),
                        _ => Err(RunError::Protocol(
                            "the model effect produced a non-model outcome".to_owned(),
                        )),
                    }
                }
                None => {
                    let permit = match self
                        .dependencies
                        .scheduler
                        .acquire(ResourceKind::Model, ctx.run_id, Some(cancel))
                        .await
                    {
                        Ok(permit) => permit,
                        Err(AcquireError::Cancelled) => {
                            super::effect_mut(effects, effect_id)
                                .set_status(EffectStatus::CancelledBeforeDispatch);
                            return Ok(ModelStep::Cancelled);
                        }
                        Err(AcquireError::QueueFull) => {
                            return Err(RunError::Protocol("scheduler queue is full".to_owned()));
                        }
                    };
                    super::effect_mut(effects, effect_id).set_status(EffectStatus::Dispatched);
                    let mut emit = move |text: String| delta(text);
                    let call = self.dependencies.dispatcher.call_model_streaming(
                        request,
                        effect_timeout,
                        &mut emit,
                    );
                    let _permit = permit;
                    match await_with_cancel(
                        await_with_deltas(call, &mut deltas, on_event),
                        Some(cancel),
                    )
                    .await
                    {
                        Some(result) => result.map(ModelStep::Output).map_err(RunError::Model),
                        None => {
                            super::effect_mut(effects, effect_id)
                                .set_status(EffectStatus::CancelledBeforeDispatch);
                            Ok(ModelStep::Cancelled)
                        }
                    }
                }
            }
        }
        .instrument(model_span)
        .await;

        match output_result {
            Ok(ModelStep::Output(output)) => {
                if let Some(usage) = output.usage {
                    on_event(&RunEvent::ModelUsage {
                        prompt_tokens: usage.prompt_tokens,
                        completion_tokens: usage.completion_tokens,
                        total_tokens: usage.total_tokens,
                    });
                }
                if self.dependencies.journal.is_none() {
                    super::effect_mut(effects, effect_id).apply_model_success(output.text.clone());
                }
                if let Some(observer) = observer {
                    if let Some(record) = effects.iter().find(|record| record.id == *effect_id) {
                        observer.on_effect_resolved(run_id, record);
                    }
                }
                Ok(ModelStep::Output(output))
            }
            Ok(ModelStep::Cancelled) => {
                if let Some(observer) = observer {
                    if let Some(record) = effects.iter().find(|record| record.id == *effect_id) {
                        observer.on_effect_resolved(run_id, record);
                    }
                }
                Ok(ModelStep::Cancelled)
            }
            Err(error) => {
                if self.dependencies.journal.is_none() {
                    super::effect_mut(effects, effect_id).apply_model_failure(error.to_string());
                }
                if let Some(observer) = observer {
                    if let Some(record) = effects.iter().find(|record| record.id == *effect_id) {
                        observer.on_effect_resolved(run_id, record);
                    }
                }
                Err(error)
            }
        }
    }

    pub(super) async fn execute_tool_batch<F>(
        &self,
        calls: Vec<(EffectId, ToolCallRequest)>,
        limit: u32,
        effect_timeout: Option<Duration>,
        cancel: &autra_core::CancelFlag,
        ctx: &mut ToolBatchContext<'_, F>,
    ) -> Result<ToolBatchOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let limit = limit.max(1) as usize;
        let run_id = ctx.run_id.clone();
        let child_execution = ChildExecutionContext {
            journal: self.dependencies.journal.clone(),
            approver: self.dependencies.approver.clone(),
            depth: 0,
        };
        let mut outcomes: Vec<Option<ToolOutcome>> = vec![None; calls.len()];
        let mut approved: Vec<Option<(EffectId, ToolCallRequest)>> = vec![None; calls.len()];
        let mut cancelled = false;

        for (index, (effect_id, call)) in calls.iter().enumerate() {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let decision = match self.authorization.policy_decision(&run_id, call) {
                Some(decision) => decision,
                None => {
                    // Unknown tools are dispatched so the outcome is a normal
                    // tool error, not an approval prompt.
                    ctx.effect_mut(effect_id)
                        .set_status(EffectStatus::Authorized);
                    if self.dependencies.journal.is_none() {
                        ctx.emit_authorized(effect_id);
                    }
                    approved[index] = Some((effect_id.clone(), call.clone()));
                    continue;
                }
            };
            if decision == autra_execution::PolicyDecision::Deny {
                let outcome =
                    super::policy_denied_outcome(call, &self.dependencies.policy.revision());
                let record = ctx.effect_mut(effect_id);
                record.set_status(EffectStatus::Rejected);
                record.error = Some(outcome.content.clone());
                if let Some(journal) = &self.dependencies.journal {
                    self.authorization
                        .persist_rejected(journal.as_ref(), &run_id, record)?;
                }
                ctx.emit_tool_result(&outcome);
                ctx.emit_rejected(effect_id);
                outcomes[index] = Some(outcome);
                continue;
            }
            if decision == autra_execution::PolicyDecision::Ask {
                ctx.effect_mut(effect_id)
                    .set_status(EffectStatus::AwaitingApproval);
                let request = self
                    .authorization
                    .approval_request(ctx.run_id, effect_id, call);
                ctx.emit_stream(&RunEvent::ApprovalRequested {
                    call_id: request.call_id.clone(),
                    tool_name: request.tool_name.clone(),
                    argument_digest: request.argument_digest.clone(),
                });
                let decision = self
                    .authorization
                    .resolve_approval(&request, effect_id, ctx.approver)
                    .await?;
                let is_approved = decision == crate::approval::ApprovalDecision::Approved;
                ctx.emit_stream(&RunEvent::ApprovalDecided {
                    call_id: request.call_id.clone(),
                    approved: is_approved,
                });
                if !is_approved {
                    let outcome = super::rejected_outcome(call);
                    let record = ctx.effect_mut(effect_id);
                    record.set_status(EffectStatus::Rejected);
                    record.error = Some(outcome.content.clone());
                    if let Some(journal) = &self.dependencies.journal {
                        self.authorization
                            .persist_rejected(journal.as_ref(), &run_id, record)?;
                    }
                    ctx.emit_tool_result(&outcome);
                    ctx.emit_rejected(effect_id);
                    outcomes[index] = Some(outcome);
                    continue;
                }
            }
            ctx.effect_mut(effect_id)
                .set_status(EffectStatus::Authorized);
            if self.dependencies.journal.is_none() {
                ctx.emit_authorized(effect_id);
            }
            approved[index] = Some((effect_id.clone(), call.clone()));
        }

        let approved_calls: Vec<(usize, EffectId, ToolCallRequest)> = approved
            .into_iter()
            .enumerate()
            .filter_map(|(index, call)| call.map(|(effect_id, call)| (index, effect_id, call)))
            .collect();

        if cancelled {
            mark_cancelled_before_dispatch(ctx, self.dependencies.journal.as_deref())?;
            return Ok(ToolBatchOutcome {
                outcomes: outcomes.into_iter().flatten().collect(),
                cancelled,
            });
        }

        if let Some(journal) = &self.dependencies.journal {
            let (was_cancelled, dispatched) = self
                .execute_claimed_tool_batch(
                    journal.as_ref(),
                    approved_calls,
                    limit,
                    effect_timeout,
                    cancel,
                    ctx,
                )
                .await?;
            cancelled = was_cancelled;
            for (index, _, outcome) in dispatched {
                outcomes[index] = Some(outcome);
            }
        } else if limit == 1 || approved_calls.len() == 1 {
            for (index, effect_id, call) in approved_calls {
                if cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let known_tool = self.dependencies.tools.get(&call.name).is_some();
                let resource = match self.dependencies.dispatcher.tool_resource(&call.name) {
                    autra_execution::ToolResource::Process => ResourceKind::Process,
                    autra_execution::ToolResource::Tool => ResourceKind::Tool,
                };
                let requirements = ctx.effect_mut(&effect_id).requirements.clone();
                let permit = match self
                    .dependencies
                    .scheduler
                    .acquire_effect(
                        resource,
                        ctx.run_id,
                        Some(cancel),
                        &requirements,
                        self.dependencies.dispatcher.tool_environment(&call.name),
                    )
                    .await
                {
                    Ok(p) => p,
                    Err(ScheduleError::Resource(AcquireError::Cancelled)) => {
                        cancelled = true;
                        break;
                    }
                    Err(e) => return Err(RunError::Protocol(e.to_string())),
                };
                ctx.effect_mut(&effect_id)
                    .set_status(EffectStatus::Dispatched);
                ctx.start_tool(&call);
                let context = autra_execution::ToolContext {
                    environment: permit.environment.clone(),
                    run_id: ctx.run_id.clone(),
                    call_id: call.call_id.clone(),
                    parent_run_id: Some(ctx.run_id.clone()),
                    cancel: Some(cancel.clone()),
                    budget: Some(ctx.budget.clone()),
                    child_execution: Some(child_execution.clone()),
                };
                let outcome = self
                    .dependencies
                    .dispatcher
                    .call_tool_with_context(&context, &call, effect_timeout)
                    .await;
                drop(permit);
                ctx.record_tool_outcome(&effect_id, known_tool, &outcome);
                ctx.emit_resolved(&effect_id);
                ctx.emit_tool_result(&outcome);
                outcomes[index] = Some(outcome);
            }
        } else {
            for chunk in approved_calls.chunks(limit) {
                let mut tasks = tokio::task::JoinSet::new();
                for (index, effect_id, call) in chunk {
                    if cancel.is_cancelled() {
                        cancelled = true;
                        break;
                    }
                    let resource = match self.dependencies.dispatcher.tool_resource(&call.name) {
                        autra_execution::ToolResource::Process => ResourceKind::Process,
                        autra_execution::ToolResource::Tool => ResourceKind::Tool,
                    };
                    let req = ctx.effect_mut(effect_id).requirements.clone();
                    let permit = match self
                        .dependencies
                        .scheduler
                        .acquire_effect(
                            resource,
                            ctx.run_id,
                            Some(cancel),
                            &req,
                            self.dependencies.dispatcher.tool_environment(&call.name),
                        )
                        .await
                    {
                        Ok(p) => p,
                        Err(ScheduleError::Resource(AcquireError::Cancelled)) => {
                            cancelled = true;
                            break;
                        }
                        Err(e) => return Err(RunError::Protocol(e.to_string())),
                    };
                    if cancel.is_cancelled() {
                        cancelled = true;
                        break;
                    }
                    let known = self.dependencies.tools.get(&call.name).is_some();
                    ctx.effect_mut(effect_id)
                        .set_status(EffectStatus::Dispatched);
                    ctx.start_tool(call);
                    let index = *index;
                    let effect_id = effect_id.clone();
                    let call = call.clone();
                    let dispatcher = self.dependencies.dispatcher.clone();
                    let context = autra_execution::ToolContext {
                        environment: permit.environment.clone(),
                        run_id: ctx.run_id.clone(),
                        call_id: call.call_id.clone(),
                        parent_run_id: Some(ctx.run_id.clone()),
                        cancel: Some(cancel.clone()),
                        budget: Some(ctx.budget.clone()),
                        child_execution: Some(child_execution.clone()),
                    };
                    // Start before waiting for another permit: the batch may
                    // be larger than the entire resource pool.
                    tasks.spawn(async move {
                        let outcome = dispatcher
                            .call_tool_with_context(&context, &call, effect_timeout)
                            .await;
                        drop(permit);
                        (index, effect_id, known, outcome)
                    });
                }
                while let Some(r) = tasks.join_next().await {
                    let (index, id, known, outcome) =
                        r.map_err(|e| RunError::Protocol(format!("tool task failed: {e}")))?;
                    ctx.record_tool_outcome(&id, known, &outcome);
                    ctx.emit_resolved(&id);
                    ctx.emit_tool_result(&outcome);
                    outcomes[index] = Some(outcome);
                }
                if cancelled || cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
            }
        }

        if cancelled || cancel.is_cancelled() {
            mark_cancelled_before_dispatch(ctx, self.dependencies.journal.as_deref())?;
            cancelled = true;
        }
        Ok(ToolBatchOutcome {
            outcomes: outcomes.into_iter().flatten().collect(),
            cancelled,
        })
    }

    async fn execute_claimed_tool_batch<F>(
        &self,
        journal: &dyn DurableTransitionStore,
        approved: Vec<(usize, EffectId, ToolCallRequest)>,
        limit: usize,
        effect_timeout: Option<Duration>,
        cancel: &autra_core::CancelFlag,
        ctx: &mut ToolBatchContext<'_, F>,
    ) -> Result<(bool, Vec<(usize, EffectId, ToolOutcome)>), RunError>
    where
        F: FnMut(&RunEvent),
    {
        let run_id = ctx.run_id.clone();
        let budget = ctx.budget.clone();
        let mut index_by_effect = std::collections::HashMap::new();
        for (index, effect_id, _) in &approved {
            let record = ctx.effect_mut(effect_id).clone();
            self.authorization
                .persist_authorized(journal, &run_id, &record)?;
            ctx.emit_authorized(effect_id);
            index_by_effect.insert(effect_id.clone(), *index);
        }
        let mut cancelled = false;
        let mut dispatched = Vec::new();
        {
            let mut on_result = |result: &DispatchResult| match &result.outcome {
                DispatchOutcome::Tool(outcome) => {
                    ctx.start_tool_outcome(outcome);
                    ctx.emit_tool_result(outcome);
                    if let Some(index) = index_by_effect.get(&result.effect_id) {
                        dispatched.push((*index, result.effect_id.clone(), outcome.clone()));
                    }
                }
                DispatchOutcome::Cancelled => cancelled = true,
                DispatchOutcome::AlreadyResolved | DispatchOutcome::Interrupted => {
                    let Ok(Some(record)) = journal.load_effect(&run_id, &result.effect_id) else {
                        return;
                    };
                    let record = &record;
                    let Some(outcome) = super::tool_outcome_from_record(record) else {
                        return;
                    };
                    ctx.start_tool_outcome(&outcome);
                    ctx.emit_tool_result(&outcome);
                    if let Some(index) = index_by_effect.get(&result.effect_id) {
                        dispatched.push((*index, result.effect_id.clone(), outcome));
                    }
                }
                DispatchOutcome::Model(_) => {}
            };
            let options = DispatchOptions {
                limit: approved.len(),
                max_parallel: limit.max(1),
                timeout: effect_timeout,
                cancel: Some(cancel),
                lease: DEFAULT_CLAIM_LEASE,
                budget: Some(budget),
                child_execution: Some(ChildExecutionContext {
                    journal: self.dependencies.journal.clone(),
                    approver: self.dependencies.approver.clone(),
                    depth: 0,
                }),
                delta: None,
                scheduler: Some(self.dependencies.scheduler.clone()),
                result_inbox: false,
            };
            OutboxDispatcher::run_once_for_run_with(
                journal,
                &run_id,
                options,
                self.dependencies.dispatcher.as_ref(),
                &mut on_result,
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()))?;
        }
        for effect_id in index_by_effect.keys() {
            super::sync_effect(journal.journal(), &run_id, effect_id, ctx.effects)?;
        }
        for (_, effect_id, _) in &dispatched {
            ctx.emit_resolved(effect_id);
        }
        Ok((cancelled || cancel.is_cancelled(), dispatched))
    }
}

fn mark_cancelled_before_dispatch<F: FnMut(&RunEvent)>(
    ctx: &mut ToolBatchContext<'_, F>,
    journal: Option<&dyn DurableTransitionStore>,
) -> Result<(), RunError> {
    for record in ctx.effects.iter_mut() {
        if matches!(
            record.status,
            EffectStatus::Recorded | EffectStatus::AwaitingApproval | EffectStatus::Authorized
        ) {
            record.set_status(EffectStatus::CancelledBeforeDispatch);
            if let Some(journal) = journal {
                journal
                    .record(ctx.run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                journal
                    .complete_outbox(&record.id, autra_execution::UNCLAIMED_CLAIM)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
        }
    }
    Ok(())
}

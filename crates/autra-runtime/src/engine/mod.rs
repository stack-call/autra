//! Local execution engine: coordinator, effect runner and recovery.
//!
//! It depends on core and execution contracts only; harness assembly belongs
//! to the product facade.

mod coordinator;
mod executor;
mod lease;
mod recovery;

pub use recovery::{RecoveryLease, RecoverySupervisor};
mod streaming;
mod validator;

use std::sync::Arc;
use std::time::Duration;

use crate::child::{ChildHandle, ChildSpec};
use crate::dispatcher::{
    DEFAULT_CLAIM_LEASE, DispatchOptions, DispatchResult, Dispatcher, OutboxDispatcher,
};
use crate::hub::RunHub;
use crate::scheduler::{GlobalScheduler, SchedulerLimits};
use autra_core::{
    AgentStepper, BudgetSnapshot, CancelFlag, EffectId, EffectRecord, EffectStatus, HistoryItem,
    RunId, ToolCallRequest, ToolOutcome,
};
use autra_execution::{
    Approver, DurableTransitionStore, EffectJournal, ObserverHook, PolicyChain, RunError, RunEvent,
    RunOptions, RunOutcome, ToolPolicy, ToolRegistry, UNCLAIMED_CLAIM,
};

/// Drives one Run: accepts events, commits transitions, executes effects and
/// resumes committed checkpoints.
#[derive(Clone)]
pub struct LocalEngine {
    dispatcher: Arc<dyn Dispatcher>,
    tools: ToolRegistry,
    journal: Option<Arc<dyn DurableTransitionStore>>,
    observer: Option<Arc<dyn ObserverHook>>,
    max_turns: u32,
    max_tool_calls: u32,
    max_parallel_tool_calls: u32,
    effect_timeout: Option<Duration>,
    scheduler: Arc<GlobalScheduler>,
    agent_profile: Option<String>,
    approver: Option<Arc<dyn Approver>>,
    policy: PolicyChain,
    actor: Option<String>,
    approval_ttl: Option<Duration>,
    hub: Option<Arc<RunHub>>,
}

impl LocalEngine {
    pub fn new(dispatcher: Arc<dyn Dispatcher>, tools: ToolRegistry) -> Self {
        Self {
            dispatcher,
            tools,
            journal: None,
            observer: None,
            max_turns: 8,
            max_tool_calls: 8,
            max_parallel_tool_calls: 1,
            effect_timeout: None,
            scheduler: Arc::new(GlobalScheduler::default()),
            agent_profile: None,
            approver: None,
            policy: PolicyChain::new(),
            actor: None,
            approval_ttl: None,
            hub: None,
        }
    }

    pub fn with_journal(mut self, journal: Arc<dyn DurableTransitionStore>) -> Self {
        self.journal = Some(journal);
        self
    }

    pub fn with_observer(mut self, observer: Arc<dyn ObserverHook>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn with_limits(
        mut self,
        max_turns: u32,
        max_tool_calls: u32,
        max_parallel_tool_calls: u32,
    ) -> Self {
        self.max_turns = max_turns;
        self.max_tool_calls = max_tool_calls;
        self.max_parallel_tool_calls = max_parallel_tool_calls.max(1);
        self
    }

    pub fn with_effect_timeout(mut self, effect_timeout: Option<Duration>) -> Self {
        self.effect_timeout = effect_timeout;
        self
    }

    /// Record the Agent configuration identity on every new Run.
    pub fn with_agent_profile(mut self, profile: impl Into<String>) -> Self {
        self.agent_profile = Some(profile.into());
        self
    }

    /// Replace the global resource limits. The scheduler is shared by every Run
    /// driven by this engine, including child Runs.
    pub fn with_scheduler_limits(mut self, limits: SchedulerLimits) -> Self {
        self.scheduler = Arc::new(GlobalScheduler::new(limits));
        self
    }

    pub fn scheduler(&self) -> &Arc<GlobalScheduler> {
        &self.scheduler
    }

    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Add a policy. Policies only narrow the host baseline.
    pub fn with_policy(mut self, policy: impl ToolPolicy + 'static) -> Self {
        self.policy = self.policy.push(policy);
        self
    }

    pub fn with_policy_chain(mut self, policy: PolicyChain) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn with_approval_ttl(mut self, ttl: Duration) -> Self {
        self.approval_ttl = Some(ttl);
        self
    }

    pub fn with_run_hub(mut self, hub: Arc<RunHub>) -> Self {
        self.hub = Some(hub);
        self
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Run one new Run from its initial event.
    pub async fn run<F>(
        &self,
        stepper: Arc<dyn AgentStepper>,
        history: &[HistoryItem],
        input: &str,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        self.run_with_identity(
            RunId::fresh(),
            None,
            self.max_budget(),
            stepper,
            history.to_vec(),
            input.to_owned(),
            options,
            on_event,
        )
        .await
    }

    /// The budget a new root Run receives from this engine.
    pub fn max_budget(&self) -> BudgetSnapshot {
        BudgetSnapshot::new(self.max_turns, self.max_tool_calls)
    }

    /// Spawn a child Run on this engine. The child gets a fresh Run id, a
    /// parent link and a cancellation flag that inherits the parent's.
    pub fn spawn_child(&self, spec: ChildSpec) -> ChildHandle {
        let run_id = RunId::fresh();
        let ChildSpec {
            stepper,
            input,
            history,
            budget,
            parent_run_id,
            cancel,
            execution,
            forward_events,
        } = spec;
        let child_cancel = CancelFlag::child_of(&cancel);
        let handle_cancel = child_cancel.clone();
        let handle_run_id = run_id.clone();
        let mut engine = self.clone();
        if let Some(journal) = execution.journal {
            engine = engine.with_journal(journal);
        }
        if let Some(approver) = execution.approver {
            engine = engine.with_approver(approver);
        }
        let (sender, receiver) = if forward_events {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let join = tokio::spawn(async move {
            let options = RunOptions {
                cancel: child_cancel,
                ..RunOptions::default()
            };
            let on_event = move |event: &RunEvent| {
                if let Some(sender) = &sender {
                    let _ = sender.send(event.clone());
                }
            };
            engine
                .run_with_identity(
                    run_id,
                    Some(parent_run_id),
                    budget,
                    stepper,
                    history,
                    input,
                    options,
                    on_event,
                )
                .await
        });
        ChildHandle::new(handle_run_id, handle_cancel, join, receiver)
    }

    /// Continue a Run from its last committed checkpoint. Requires a journal.
    pub async fn drain_outbox(&self, limit: usize) -> Result<Vec<DispatchResult>, RunError> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| RunError::Protocol("outbox drain requires a journal".to_owned()))?;
        let run_ids = journal
            .journal()
            .list_run_ids()
            .map_err(|error| RunError::Journal(error.to_string()))?;
        if run_ids.is_empty() {
            return OutboxDispatcher::run_due(
                journal.journal(),
                limit,
                self.dispatcher.as_ref(),
                self.effect_timeout,
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()));
        }
        let mut results = Vec::new();
        for run_id in run_ids {
            if results.len() >= limit {
                break;
            }
            if journal
                .journal()
                .run_lease(&run_id)
                .map_err(|error| RunError::Journal(error.to_string()))?
                .is_some_and(|lease| lease.is_live(autra_execution::now_unix_ms()))
            {
                // A live executor owns this Run; its own loop drains it.
                continue;
            }
            let options = DispatchOptions {
                limit: limit - results.len(),
                max_parallel: 1,
                timeout: self.effect_timeout,
                cancel: None,
                lease: DEFAULT_CLAIM_LEASE,
                budget: None,
                child_execution: None,
                delta: None,
                scheduler: Some(self.scheduler.clone()),
            };
            let mut batch = OutboxDispatcher::run_once_for_run_with(
                journal.journal(),
                &run_id,
                options,
                self.dispatcher.as_ref(),
                |_| {},
            )
            .await
            .map_err(|error| RunError::Journal(error.to_string()))?;
            results.append(&mut batch);
        }
        Ok(results)
    }
}

fn tool_outcome_from_record(record: &EffectRecord) -> Option<ToolOutcome> {
    let call_id = record.call_id.clone()?;
    let name = record.tool_name.clone()?;
    let (content, is_error, outcome_unknown) = match record.status {
        EffectStatus::Committed => (record.result.clone().unwrap_or_default(), false, false),
        EffectStatus::Failed | EffectStatus::Rejected => (
            record
                .error
                .clone()
                .unwrap_or_else(|| "the effect failed".to_owned()),
            true,
            false,
        ),
        EffectStatus::OutcomeUnknown => (
            record
                .error
                .clone()
                .unwrap_or_else(|| "the effect outcome is unknown".to_owned()),
            true,
            true,
        ),
        EffectStatus::CancelledBeforeDispatch => (
            "the effect was cancelled before dispatch".to_owned(),
            true,
            false,
        ),
        _ => return None,
    };
    Some(ToolOutcome {
        call_id,
        name,
        content,
        is_error,
        outcome_unknown,
    })
}

fn sync_effects(
    journal: &dyn EffectJournal,
    run_id: &RunId,
    effects: &mut [EffectRecord],
) -> Result<(), RunError> {
    let stored = journal
        .load(run_id)
        .map_err(|error| RunError::Journal(error.to_string()))?;
    for record in effects.iter_mut() {
        if let Some(updated) = stored.iter().find(|stored| stored.id == record.id) {
            *record = updated.clone();
        }
    }
    Ok(())
}

fn policy_denied_outcome(call: &ToolCallRequest, revision: &str) -> ToolOutcome {
    ToolOutcome {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        content: format!("denied by policy `{revision}`; the tool was not executed"),
        is_error: true,
        outcome_unknown: false,
    }
}

fn rejected_outcome(call: &ToolCallRequest) -> ToolOutcome {
    ToolOutcome {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        content: "approval rejected; the tool was not executed".to_owned(),
        is_error: true,
        outcome_unknown: false,
    }
}

fn effect_mut<'a>(effects: &'a mut [EffectRecord], effect_id: &EffectId) -> &'a mut EffectRecord {
    effects
        .iter_mut()
        .find(|record| &record.id == effect_id)
        .expect("effect record must exist")
}

fn cancel_staged_effects(
    effects: &mut [EffectRecord],
    journal: Option<&dyn DurableTransitionStore>,
    run_id: &RunId,
) -> Result<(), RunError> {
    for record in effects.iter_mut() {
        if matches!(
            record.status,
            EffectStatus::Recorded | EffectStatus::AwaitingApproval | EffectStatus::Authorized
        ) {
            record.set_status(EffectStatus::CancelledBeforeDispatch);
            if let Some(journal) = journal {
                journal
                    .record(run_id, record)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
                journal
                    .complete_outbox(&record.id, UNCLAIMED_CLAIM)
                    .map_err(|error| RunError::Journal(error.to_string()))?;
            }
        }
    }
    Ok(())
}

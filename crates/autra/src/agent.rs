//! Product facade over the local execution engine.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use autra_core::{AgentStepper, HistoryItem, RunId};
use autra_execution::{
    Approver, DurableTransitionStore, ModelProvider, ObserverHook, Tool, ToolRegistry,
};
use autra_harness::{
    ContextBuilder, LoadSkillTool, PlanItem, ReActStepper, SharedPlan, SimpleContextBuilder, Skill,
    SkillRegistry, UpdatePlanTool,
};
use thiserror::Error;

use crate::subagent::{SubagentRegistry, SubagentTool};
use autra_runtime::{DispatchResult, Dispatcher, LocalDispatcher, LocalEngine};

pub use autra_execution::{RunError, RunEvent, RunOptions, RunOutcome, RunStatus};

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("agent `{name}` requires a model")]
    MissingModel { name: String },
}

pub struct Agent {
    name: String,
    instructions: Option<String>,
    skills: SkillRegistry,
    context_budget_chars: Option<usize>,
    plan: Option<SharedPlan>,
    context_builder: Option<Arc<dyn ContextBuilder>>,
    stepper: Option<Arc<dyn AgentStepper>>,
    engine: LocalEngine,
}

impl Agent {
    pub fn builder(name: impl Into<String>) -> AgentBuilder {
        AgentBuilder::new(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn plan(&self) -> Option<Vec<PlanItem>> {
        self.plan
            .as_ref()
            .map(|plan| plan.lock().expect("plan lock poisoned").clone())
    }

    pub async fn run(&self, input: &str) -> Result<RunOutcome, RunError> {
        self.run_with_options(input, RunOptions::default(), |_| {})
            .await
    }

    pub async fn run_with_options<F>(
        &self,
        input: &str,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        self.run_with_history(&[], input, options, on_event).await
    }

    #[tracing::instrument(skip_all, fields(agent = %self.name))]
    pub async fn run_with_history<F>(
        &self,
        history: &[HistoryItem],
        input: &str,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let stepper = self.stepper_for_run();
        self.engine
            .run(stepper, history, input, options, on_event)
            .await
    }

    /// Continue a Run from its last committed checkpoint. Requires a journal.
    pub async fn resume(&self, run_id: &RunId) -> Result<RunOutcome, RunError> {
        self.resume_with_options(run_id, RunOptions::default(), |_| {})
            .await
    }

    #[tracing::instrument(skip_all, fields(agent = %self.name, run_id = %run_id))]
    pub async fn resume_with_options<F>(
        &self,
        run_id: &RunId,
        options: RunOptions,
        on_event: F,
    ) -> Result<RunOutcome, RunError>
    where
        F: FnMut(&RunEvent),
    {
        let stepper = self.stepper_for_run();
        self.engine.resume(stepper, run_id, options, on_event).await
    }

    /// Drain due outbox rows across all Runs, reclaiming expired leases.
    pub async fn drain_outbox(&self, limit: usize) -> Result<Vec<DispatchResult>, RunError> {
        self.engine.drain_outbox(limit).await
    }

    fn stepper_for_run(&self) -> Arc<dyn AgentStepper> {
        match &self.stepper {
            Some(stepper) => stepper.clone(),
            None => {
                let context_builder: Arc<dyn ContextBuilder> = match &self.context_builder {
                    Some(builder) => builder.clone(),
                    None => {
                        let mut builder = SimpleContextBuilder::new(self.skills.index());
                        if let Some(max_chars) = self.context_budget_chars {
                            builder = builder.with_max_chars(max_chars);
                        }
                        Arc::new(builder)
                    }
                };
                Arc::new(
                    ReActStepper::new(self.instructions.clone(), self.engine.tools().specs())
                        .with_shared_context_builder(context_builder),
                )
            }
        }
    }
}

pub struct AgentBuilder {
    name: String,
    instructions: Option<String>,
    model: Option<Arc<dyn ModelProvider>>,
    dispatcher: Option<Arc<dyn Dispatcher>>,
    tools: ToolRegistry,
    effect_timeout: Option<Duration>,
    max_turns: u32,
    max_tool_calls: u32,
    max_parallel_tool_calls: u32,
    approver: Option<Arc<dyn Approver>>,
    skills: SkillRegistry,
    context_budget_chars: Option<usize>,
    planning: bool,
    subagents: SubagentRegistry,
    context_builder: Option<Arc<dyn ContextBuilder>>,
    stepper: Option<Arc<dyn AgentStepper>>,
    observer: Option<Arc<dyn ObserverHook>>,
    journal: Option<Arc<dyn DurableTransitionStore>>,
}

impl AgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: None,
            model: None,
            dispatcher: None,
            tools: ToolRegistry::new(),
            effect_timeout: None,
            max_turns: 8,
            max_tool_calls: 8,
            max_parallel_tool_calls: 1,
            approver: None,
            skills: SkillRegistry::new(),
            context_budget_chars: None,
            planning: false,
            subagents: SubagentRegistry::new(),
            context_builder: None,
            stepper: None,
            observer: None,
            journal: None,
        }
    }

    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub fn model(mut self, model: impl ModelProvider + 'static) -> Self {
        self.model = Some(Arc::new(model));
        self
    }

    pub fn effect_timeout(mut self, effect_timeout: Duration) -> Self {
        self.effect_timeout = Some(effect_timeout);
        self
    }

    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    pub fn tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.register(tool);
        self
    }

    pub fn max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub fn approver(mut self, approver: impl Approver + 'static) -> Self {
        self.approver = Some(Arc::new(approver));
        self
    }

    pub fn max_parallel_tool_calls(mut self, max_parallel_tool_calls: u32) -> Self {
        self.max_parallel_tool_calls = max_parallel_tool_calls.max(1);
        self
    }

    pub fn max_tool_calls(mut self, max_tool_calls: u32) -> Self {
        self.max_tool_calls = max_tool_calls;
        self
    }

    pub fn skill(mut self, skill: Skill) -> Self {
        self.skills.register(skill);
        self
    }

    pub fn subagent(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        agent: Agent,
    ) -> Self {
        self.subagents.register(name, description, agent);
        self
    }

    pub fn planning(mut self) -> Self {
        self.planning = true;
        self
    }

    /// Use a custom ContextBuilder. The builder owns skills and budget composition.
    pub fn context_builder(mut self, builder: impl ContextBuilder + 'static) -> Self {
        self.context_builder = Some(Arc::new(builder));
        self
    }

    /// Replace the ReAct Stepper. The custom engine owns its decisions; the
    /// coordinator still dispatches emitted intents and enforces budgets.
    pub fn stepper(mut self, stepper: impl AgentStepper + 'static) -> Self {
        self.stepper = Some(Arc::new(stepper));
        self
    }

    pub fn observer(mut self, observer: impl ObserverHook + 'static) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    pub fn dispatcher(mut self, dispatcher: impl Dispatcher + 'static) -> Self {
        self.dispatcher = Some(Arc::new(dispatcher));
        self
    }

    pub fn journal(mut self, journal: impl DurableTransitionStore + 'static) -> Self {
        self.journal = Some(Arc::new(journal));
        self
    }

    pub fn context_budget_chars(mut self, max_chars: usize) -> Self {
        self.context_budget_chars = Some(max_chars);
        self
    }

    pub fn build(self) -> Result<Agent, BuildError> {
        let mut tools = self.tools;
        if !self.skills.is_empty() {
            tools.register(LoadSkillTool::new(Arc::new(self.skills.clone())));
        }
        if !self.subagents.is_empty() {
            tools.register(SubagentTool::new(self.subagents.clone()));
        }
        let (instructions, plan) = if self.planning {
            let plan: SharedPlan = Arc::new(Mutex::new(Vec::new()));
            tools.register(UpdatePlanTool::new(plan.clone()));
            let instructions = match self.instructions {
                Some(instructions) => format!(
                    "{instructions}

Maintain a short plan with `update_plan` and keep it current."
                ),
                None => "Maintain a short plan with `update_plan` and keep it current.".to_owned(),
            };
            (Some(instructions), Some(plan))
        } else {
            (self.instructions, None)
        };
        let dispatcher = match self.dispatcher {
            Some(dispatcher) => dispatcher,
            None => {
                let model = self.model.ok_or_else(|| BuildError::MissingModel {
                    name: self.name.clone(),
                })?;
                Arc::new(LocalDispatcher::new(model, tools.clone()))
            }
        };
        let mut engine = LocalEngine::new(dispatcher, tools)
            .with_limits(
                self.max_turns,
                self.max_tool_calls,
                self.max_parallel_tool_calls,
            )
            .with_effect_timeout(self.effect_timeout);
        if let Some(journal) = self.journal {
            engine = engine.with_journal(journal);
        }
        if let Some(observer) = self.observer {
            engine = engine.with_observer(observer);
        }
        if let Some(approver) = self.approver {
            engine = engine.with_approver(approver);
        }
        Ok(Agent {
            name: self.name,
            instructions,
            skills: self.skills,
            context_budget_chars: self.context_budget_chars,
            plan,
            context_builder: self.context_builder,
            stepper: self.stepper,
            engine,
        })
    }
}

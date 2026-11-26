use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra_core::{EffectClass, RunId, ToolSpec};
use autra_execution::{ToolContext, ToolInput, ToolOutput};
use serde::Deserialize;

use crate::tools::{Tool, ToolError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

impl PlanStatus {
    fn marker(self) -> &'static str {
        match self {
            Self::Pending => " ",
            Self::InProgress => "~",
            Self::Completed => "x",
        }
    }

    fn parse(value: &str) -> Result<Self, ToolError> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            other => Err(ToolError::new(format!(
                "invalid plan status `{other}`; use pending, in_progress or completed"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanItem {
    pub title: String,
    pub status: PlanStatus,
}

/// Run-scoped plan storage.
///
/// A plan belongs to one Run; two Runs sharing an Agent must not overwrite each
/// other. Persistence, if needed, belongs in an explicit Run/Session state
/// protocol rather than this in-memory map.
#[derive(Clone, Default)]
pub struct PlanStore {
    plans: Arc<Mutex<HashMap<RunId, Vec<PlanItem>>>>,
}

impl PlanStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, run_id: &RunId) -> Option<Vec<PlanItem>> {
        self.plans
            .lock()
            .expect("plan lock poisoned")
            .get(run_id)
            .cloned()
    }

    pub fn replace(&self, run_id: &RunId, plan: Vec<PlanItem>) {
        self.plans
            .lock()
            .expect("plan lock poisoned")
            .insert(run_id.clone(), plan);
    }
}

/// Compatibility alias for callers that named the old shared plan type.
pub type SharedPlan = PlanStore;

pub struct UpdatePlanTool {
    plans: PlanStore,
}

impl UpdatePlanTool {
    pub fn new(plans: PlanStore) -> Self {
        Self { plans }
    }

    pub fn plan(&self, run_id: &RunId) -> Option<Vec<PlanItem>> {
        self.plans.get(run_id)
    }
}

#[derive(Deserialize)]
struct UpdatePlanArgs {
    items: Vec<PlanItemArg>,
}

#[derive(Deserialize)]
struct PlanItemArg {
    title: String,
    status: String,
}

const UPDATE_PLAN_SCHEMA: &str = r#"{"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"title":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["title","status"]}}},"required":["items"]}"#;

#[async_trait]
impl Tool for UpdatePlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "update_plan".to_owned(),
            description: "Replace the current plan with a short checklist.".to_owned(),
            input_schema: UPDATE_PLAN_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        Err(ToolError::new(
            "update_plan requires a run context and cannot be called directly",
        ))
    }

    async fn execute(
        &self,
        input: ToolInput,
        context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: UpdatePlanArgs = serde_json::from_str(&input.arguments)
            .map_err(|error| ToolError::new(format!("invalid update_plan arguments: {error}")))?;
        let mut plan = Vec::with_capacity(args.items.len());
        for item in args.items {
            if item.title.trim().is_empty() {
                return Err(ToolError::new("plan item title must not be empty"));
            }
            plan.push(PlanItem {
                title: item.title,
                status: PlanStatus::parse(&item.status)?,
            });
        }
        let rendered = render(&plan);
        self.plans.replace(&context.run_id, plan);
        Ok(ToolOutput::text(rendered))
    }
}

fn render(items: &[PlanItem]) -> String {
    if items.is_empty() {
        return "(empty plan)".to_owned();
    }
    items
        .iter()
        .map(|item| format!("- [{}] {}", item.status.marker(), item.title))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{PlanItem, PlanStatus, PlanStore, UpdatePlanTool};
    use crate::tools::Tool;
    use autra_core::RunId;
    use autra_execution::{ToolContext, ToolInput};

    fn context(run_id: &str) -> ToolContext {
        ToolContext {
            environment: None,
            run_id: RunId::new(run_id),
            call_id: "call-1".to_owned(),
            parent_run_id: None,
            cancel: None,
            budget: None,
            child_execution: None,
        }
    }

    async fn update(tool: &UpdatePlanTool, run_id: &str, arguments: &str) -> String {
        tool.execute(
            ToolInput {
                arguments: arguments.to_owned(),
            },
            context(run_id),
        )
        .await
        .unwrap()
        .content
    }

    #[tokio::test]
    async fn replaces_and_renders_the_plan() {
        let plans = PlanStore::new();
        let tool = UpdatePlanTool::new(plans.clone());

        let output = update(
            &tool,
            "run-1",
            r#"{"items":[{"title":"read","status":"completed"},{"title":"write","status":"in_progress"}]}"#,
        )
        .await;

        assert!(output.contains("- [x] read"));
        assert!(output.contains("- [~] write"));
        assert_eq!(
            tool.plan(&RunId::new("run-1")),
            Some(vec![
                PlanItem {
                    title: "read".to_owned(),
                    status: PlanStatus::Completed
                },
                PlanItem {
                    title: "write".to_owned(),
                    status: PlanStatus::InProgress
                }
            ])
        );
    }

    #[tokio::test]
    async fn plans_are_isolated_per_run() {
        let plans = PlanStore::new();
        let tool = UpdatePlanTool::new(plans.clone());

        update(
            &tool,
            "run-1",
            r#"{"items":[{"title":"one","status":"pending"}]}"#,
        )
        .await;
        update(
            &tool,
            "run-2",
            r#"{"items":[{"title":"two","status":"completed"}]}"#,
        )
        .await;

        assert_eq!(
            tool.plan(&RunId::new("run-1")).unwrap()[0].title,
            "one".to_owned()
        );
        assert_eq!(
            tool.plan(&RunId::new("run-2")).unwrap()[0].title,
            "two".to_owned()
        );
    }

    #[tokio::test]
    async fn empty_plan_is_explicit() {
        let tool = UpdatePlanTool::new(PlanStore::new());

        let output = update(&tool, "run-1", r#"{"items":[]}"#).await;

        assert_eq!(output, "(empty plan)");
    }

    #[tokio::test]
    async fn invalid_status_is_an_error() {
        let tool = UpdatePlanTool::new(PlanStore::new());

        let error = tool
            .execute(
                ToolInput {
                    arguments: r#"{"items":[{"title":"x","status":"later"}]}"#.to_owned(),
                },
                context("run-1"),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("invalid plan status"));
    }
}

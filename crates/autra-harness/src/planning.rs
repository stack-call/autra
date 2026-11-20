use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
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

pub type SharedPlan = Arc<Mutex<Vec<PlanItem>>>;

pub struct UpdatePlanTool {
    plan: SharedPlan,
}

impl UpdatePlanTool {
    pub fn new(plan: SharedPlan) -> Self {
        Self { plan }
    }

    pub fn plan(&self) -> Vec<PlanItem> {
        self.plan.lock().expect("plan lock poisoned").clone()
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

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: UpdatePlanArgs = serde_json::from_str(arguments)
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
        *self.plan.lock().expect("plan lock poisoned") = plan;
        Ok(rendered)
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
    use super::{PlanItem, PlanStatus, UpdatePlanTool};
    use crate::tools::Tool;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn replaces_and_renders_the_plan() {
        let plan = Arc::new(Mutex::new(Vec::new()));
        let tool = UpdatePlanTool::new(plan.clone());

        let output = tool
            .call(
                r#"{"items":[{"title":"read","status":"completed"},{"title":"write","status":"in_progress"}]}"#,
            )
            .await
            .unwrap();

        assert!(output.contains("- [x] read"));
        assert!(output.contains("- [~] write"));
        assert_eq!(
            tool.plan(),
            vec![
                PlanItem {
                    title: "read".to_owned(),
                    status: PlanStatus::Completed
                },
                PlanItem {
                    title: "write".to_owned(),
                    status: PlanStatus::InProgress
                }
            ]
        );
    }

    #[tokio::test]
    async fn empty_plan_is_explicit() {
        let plan = Arc::new(Mutex::new(Vec::new()));
        let tool = UpdatePlanTool::new(plan);

        let output = tool.call(r#"{"items":[]}"#).await.unwrap();

        assert_eq!(output, "(empty plan)");
    }

    #[tokio::test]
    async fn invalid_status_is_an_error() {
        let plan = Arc::new(Mutex::new(Vec::new()));
        let tool = UpdatePlanTool::new(plan);

        let error = tool
            .call(r#"{"items":[{"title":"x","status":"later"}]}"#)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("invalid plan status"));
    }
}

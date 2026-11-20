use async_trait::async_trait;
use autra_core::{EffectClass, ModelOutput, ToolCallRequest, ToolSpec};
use autra_harness::{
    FakeModel, TOOL_PROTOCOL_VERSION, Tool, ToolContext, ToolContract, ToolError, ToolInput,
    ToolOutput,
};
use autra_runtime::{Agent, ApprovalDecision, ApprovalRequest, Approver, RunEvent, RunOptions};

struct ContractTool;

#[async_trait]
impl Tool for ContractTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "custom".to_owned(),
            description: "A contract-aware tool".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn contract(&self) -> ToolContract {
        ToolContract {
            effect_class: EffectClass::ReadOnly,
            requires_approval: true,
        }
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        Err(ToolError::new("call() should not be used"))
    }

    async fn execute(
        &self,
        input: ToolInput,
        context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(format!(
            "args={} call_id={}",
            input.arguments, context.call_id
        )))
    }
}

struct AlwaysApprove;

#[async_trait]
impl Approver for AlwaysApprove {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Approved
    }
}

#[tokio::test]
async fn runtime_uses_contract_and_execute() {
    let model = FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "custom".to_owned(),
                arguments: "{}".to_owned(),
            }],
        },
        ModelOutput {
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = Agent::builder("contract")
        .model(model)
        .tool(ContractTool)
        .approver(AlwaysApprove)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let tool = outcome
        .state()
        .history
        .iter()
        .find_map(|item| match item {
            autra_harness::HistoryItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool.content, "args={} call_id=call-1");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunEvent::ApprovalRequested { .. }))
    );
    assert_eq!(ContractTool.protocol_version(), TOOL_PROTOCOL_VERSION);
    assert_eq!(ContractTool.descriptor().name, "custom");
}

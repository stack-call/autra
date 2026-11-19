use serde::{Deserialize, Serialize};

/// A model-requested tool call. Arguments are a JSON object encoded as text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

/// The accepted outcome of one tool call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    #[serde(default)]
    pub outcome_unknown: bool,
}

/// A tool description visible to the model. `input_schema` is a JSON Schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

/// The authoritative history of one run, in accepted order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryItem {
    User(String),
    Assistant {
        text: Option<String>,
        tool_calls: Vec<ToolCallRequest>,
    },
    Tool(ToolOutcome),
}

/// One complete model response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOutput {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCallRequest>,
}

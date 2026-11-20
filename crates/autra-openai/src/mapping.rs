use autra_core::{HistoryItem, ModelCallSpec, ModelOutput, ToolCallRequest};
use autra_harness::ModelError;
use std::collections::BTreeMap;

use serde_json::{Value, json};

pub(crate) fn build_request(
    model: &str,
    request: &ModelCallSpec,
    stream: bool,
) -> Result<Value, ModelError> {
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        if !system.is_empty() {
            messages.push(json!({ "role": "system", "content": system }));
        }
    }
    for item in &request.messages {
        messages.push(history_to_message(item));
    }

    let mut body = json!({ "model": model, "messages": messages });
    if stream {
        body["stream"] = json!(true);
    }
    if !request.tools.is_empty() {
        let mut tools = Vec::new();
        for tool in &request.tools {
            let parameters: Value = serde_json::from_str(&tool.input_schema).map_err(|error| {
                ModelError::new(format!(
                    "invalid input schema for tool `{}`: {error}",
                    tool.name
                ))
            })?;
            tools.push(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters
                }
            }));
        }
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = json!("auto");
    }
    Ok(body)
}

fn history_to_message(item: &HistoryItem) -> Value {
    match item {
        HistoryItem::User(text) => json!({ "role": "user", "content": text }),
        HistoryItem::Assistant { text, tool_calls } => {
            let mut message = json!({ "role": "assistant", "content": text });
            if !tool_calls.is_empty() {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.call_id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.arguments
                            }
                        })
                    })
                    .collect();
                message["tool_calls"] = Value::Array(calls);
            }
            message
        }
        HistoryItem::Tool(outcome) => json!({
            "role": "tool",
            "tool_call_id": outcome.call_id,
            "content": outcome.content
        }),
    }
}

pub(crate) fn parse_response(value: &Value) -> Result<ModelOutput, ModelError> {
    let message = value
        .pointer("/choices/0/message")
        .ok_or_else(|| ModelError::new("model response has no choices[0].message"))?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| ModelError::new("tool call is missing id"))?;
            let function = call
                .get("function")
                .ok_or_else(|| ModelError::new("tool call is missing function"))?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ModelError::new("tool call is missing function name"))?;
            let arguments = match function.get("arguments") {
                Some(Value::String(arguments)) => arguments.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            tool_calls.push(ToolCallRequest {
                call_id: call_id.to_owned(),
                name: name.to_owned(),
                arguments,
            });
        }
    }

    Ok(ModelOutput { text, tool_calls })
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    let mut truncated: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        truncated.push('…');
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::{build_request, parse_response};
    use autra_core::{HistoryItem, ModelCallSpec, ToolCallRequest, ToolOutcome, ToolSpec};
    use serde_json::json;

    fn call() -> ToolCallRequest {
        ToolCallRequest {
            call_id: "call-1".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"text":"hi"}"#.to_owned(),
        }
    }

    #[test]
    fn builds_messages_and_tools() {
        let request = ModelCallSpec {
            system: Some("be brief".to_owned()),
            messages: vec![
                HistoryItem::User("hello".to_owned()),
                HistoryItem::Assistant {
                    text: None,
                    tool_calls: vec![call()],
                },
                HistoryItem::Tool(ToolOutcome {
                    call_id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                    content: "hi".to_owned(),
                    is_error: false,
                    outcome_unknown: false,
                }),
            ],
            tools: vec![ToolSpec {
                name: "echo".to_owned(),
                description: "Echo".to_owned(),
                input_schema: r#"{"type":"object"}"#.to_owned(),
            }],
        };

        let body = build_request("test-model", &request, false).unwrap();

        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][2]["role"], "assistant");
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["messages"][3]["tool_call_id"], "call-1");
        assert_eq!(body["tools"][0]["function"]["name"], "echo");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn parses_text_and_tool_calls() {
        let value = json!({
            "choices": [{
                "message": {
                    "content": "thinking",
                    "tool_calls": [{
                        "id": "call-9",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    }]
                }
            }]
        });

        let output = parse_response(&value).unwrap();

        assert_eq!(output.text.as_deref(), Some("thinking"));
        assert_eq!(output.tool_calls.len(), 1);
        assert_eq!(output.tool_calls[0].call_id, "call-9");
        assert_eq!(output.tool_calls[0].name, "read_file");
    }

    #[test]
    fn object_arguments_are_serialized() {
        let value = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call-1",
                        "function": { "name": "echo", "arguments": { "text": "hi" } }
                    }]
                }
            }]
        });

        let output = parse_response(&value).unwrap();

        assert_eq!(output.tool_calls[0].arguments, r#"{"text":"hi"}"#);
    }

    #[test]
    fn empty_message_is_allowed() {
        let value = json!({ "choices": [{ "message": {} }] });

        let output = parse_response(&value).unwrap();

        assert_eq!(output.text, None);
        assert!(output.tool_calls.is_empty());
    }

    #[test]
    fn missing_message_is_an_error() {
        let value = json!({ "choices": [] });

        assert!(parse_response(&value).is_err());
    }

    #[test]
    fn invalid_tool_schema_is_an_error() {
        let request = ModelCallSpec {
            system: None,
            messages: Vec::new(),
            tools: vec![ToolSpec {
                name: "broken".to_owned(),
                description: "Broken".to_owned(),
                input_schema: "not json".to_owned(),
            }],
        };

        assert!(build_request("test-model", &request, false).is_err());
    }
}

#[cfg(test)]
mod null_argument_tests {
    use super::parse_response;
    use serde_json::json;

    #[test]
    fn null_arguments_become_empty() {
        let value = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call-1",
                        "function": { "name": "echo", "arguments": null }
                    }]
                }
            }]
        });

        let output = parse_response(&value).unwrap();

        assert_eq!(output.tool_calls[0].arguments, "");
    }
}

#[derive(Default)]
pub(crate) struct StreamAccumulator {
    pub content: String,
    pub tool_calls: BTreeMap<usize, StreamToolCall>,
}

#[derive(Default)]
pub(crate) struct StreamToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Apply one `data:` payload from an OpenAI-compatible SSE stream. Returns the
/// content delta, if any.
pub(crate) fn apply_stream_data(
    data: &str,
    accumulator: &mut StreamAccumulator,
) -> Result<Option<String>, ModelError> {
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(data).map_err(|error| {
        ModelError::new(format!(
            "invalid model stream chunk: {error}: {}",
            truncate(data, 200)
        ))
    })?;
    if let Some(error) = value.get("error") {
        return Err(ModelError::new(format!("model stream error: {error}")));
    }
    let mut delta_text = None;
    if let Some(text) = value
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
    {
        accumulator.content.push_str(text);
        delta_text = Some(text.to_owned());
    }
    if let Some(calls) = value
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let entry = accumulator.tool_calls.entry(index).or_default();
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                entry.id = id.to_owned();
            }
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                entry.name.push_str(name);
            }
            if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str) {
                entry.arguments.push_str(arguments);
            }
        }
    }
    Ok(delta_text)
}

pub(crate) fn finish_stream(accumulator: StreamAccumulator) -> ModelOutput {
    let text = if accumulator.content.is_empty() {
        None
    } else {
        Some(accumulator.content)
    };
    let tool_calls = accumulator
        .tool_calls
        .into_values()
        .filter(|call| !call.name.is_empty() || !call.arguments.is_empty())
        .map(|call| ToolCallRequest {
            call_id: call.id,
            name: call.name,
            arguments: call.arguments,
        })
        .collect();
    ModelOutput { text, tool_calls }
}

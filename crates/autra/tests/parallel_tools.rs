use autra::react_history;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use autra::{Agent, RunEvent, RunOptions, RunOutcome, RunStatus};
use autra_core::{CancelFlag, ModelCallSpec, ModelOutput, ToolCallRequest, ToolSpec};
use autra_harness::{FakeModel, HistoryItem, ModelError, ModelProvider, Tool, ToolError};

struct DelayTool {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    cancel: Option<CancelFlag>,
}

#[async_trait]
impl Tool for DelayTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delay".to_owned(),
            description: "Sleeps and records concurrency.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> autra_core::EffectClass {
        autra_core::EffectClass::ReadOnly
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let value: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|error| ToolError::new(format!("invalid delay arguments: {error}")))?;
        let text = value["text"].as_str().unwrap_or_default().to_owned();
        let delay = value["delay_ms"].as_u64().unwrap_or(0);
        if value["cancel_on_call"].as_bool().unwrap_or(false) {
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
        }
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(delay)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(text)
    }
}

fn delay_call(id: &str, arguments: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: id.to_owned(),
        name: "delay".to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn tool_model(calls: Vec<ToolCallRequest>) -> FakeModel {
    FakeModel::new(vec![
        ModelOutput {
            usage: None,
            text: None,
            tool_calls: calls,
        },
        ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        },
    ])
}

fn tool_contents(outcome: &RunOutcome) -> Vec<String> {
    react_history(outcome.state())
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            HistoryItem::Tool(tool) => Some(tool.content),
            _ => None,
        })
        .collect()
}

fn started_ids(events: &[RunEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            RunEvent::ToolCallStarted { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect()
}

fn delay_tool(
    active: &Arc<AtomicUsize>,
    max_active: &Arc<AtomicUsize>,
    cancel: Option<CancelFlag>,
) -> DelayTool {
    DelayTool {
        active: active.clone(),
        max_active: max_active.clone(),
        cancel,
    }
}

#[tokio::test]
async fn parallel_batch_runs_concurrently_and_keeps_model_order() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let model = tool_model(vec![
        delay_call("call-1", r#"{"text":"first","delay_ms":100}"#),
        delay_call("call-2", r#"{"text":"second","delay_ms":10}"#),
    ]);
    let agent = Agent::builder("parallel")
        .model(model)
        .tool(delay_tool(&active, &max_active, None))
        .max_parallel_tool_calls(2)
        .build()
        .unwrap();

    let mut events = Vec::new();
    let outcome = agent
        .run_with_options("go", RunOptions::default(), |event| {
            events.push(event.clone())
        })
        .await
        .unwrap();

    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(
        tool_contents(&outcome),
        ["first".to_owned(), "second".to_owned()]
    );
    assert_eq!(result_ids(&events), ["call-2", "call-1"]);
}

#[tokio::test]
async fn limit_one_stays_sequential() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let model = tool_model(vec![
        delay_call("call-1", r#"{"text":"first","delay_ms":20}"#),
        delay_call("call-2", r#"{"text":"second","delay_ms":10}"#),
    ]);
    let agent = Agent::builder("sequential")
        .model(model)
        .tool(delay_tool(&active, &max_active, None))
        .max_parallel_tool_calls(1)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    assert_eq!(
        tool_contents(&outcome),
        ["first".to_owned(), "second".to_owned()]
    );
}

#[tokio::test]
async fn run_options_override_the_builder_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let model = tool_model(vec![
        delay_call("call-1", r#"{"text":"first","delay_ms":100}"#),
        delay_call("call-2", r#"{"text":"second","delay_ms":10}"#),
    ]);
    let agent = Agent::builder("override")
        .model(model)
        .tool(delay_tool(&active, &max_active, None))
        .max_parallel_tool_calls(1)
        .build()
        .unwrap();
    let options = RunOptions {
        max_parallel_tool_calls: Some(2),
        ..RunOptions::default()
    };

    let outcome = agent.run_with_options("go", options, |_| {}).await.unwrap();

    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(
        tool_contents(&outcome),
        ["first".to_owned(), "second".to_owned()]
    );
}

struct CancellingModel {
    cancel: CancelFlag,
    calls: Vec<ToolCallRequest>,
}

#[async_trait]
impl ModelProvider for CancellingModel {
    async fn complete(&self, _request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.cancel.cancel();
        Ok(ModelOutput {
            usage: None,
            text: None,
            tool_calls: self.calls.clone(),
        })
    }
}

#[tokio::test]
async fn cancellation_before_a_batch_dispatches_no_tools() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let cancel = CancelFlag::new();
    let model = CancellingModel {
        cancel: cancel.clone(),
        calls: vec![
            delay_call("call-1", r#"{"text":"first","delay_ms":10}"#),
            delay_call("call-2", r#"{"text":"second","delay_ms":10}"#),
        ],
    };
    let agent = Agent::builder("cancel-before")
        .model(model)
        .tool(delay_tool(&active, &max_active, None))
        .max_parallel_tool_calls(2)
        .build()
        .unwrap();
    let options = RunOptions {
        cancel: cancel.clone(),
        ..RunOptions::default()
    };
    let mut events = Vec::new();

    let outcome = agent
        .run_with_options("go", options, |event| events.push(event.clone()))
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
    assert_eq!(max_active.load(Ordering::SeqCst), 0);
    assert!(started_ids(&events).is_empty());
}

#[tokio::test]
async fn cancellation_mid_batch_records_in_flight_calls_and_starts_no_later_chunk() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let cancel = CancelFlag::new();
    let model = tool_model(vec![
        delay_call(
            "call-1",
            r#"{"text":"first","delay_ms":50,"cancel_on_call":true}"#,
        ),
        delay_call("call-2", r#"{"text":"second","delay_ms":10}"#),
        delay_call("call-3", r#"{"text":"third","delay_ms":10}"#),
    ]);
    let agent = Agent::builder("cancel-during")
        .model(model)
        .tool(delay_tool(&active, &max_active, Some(cancel.clone())))
        .max_parallel_tool_calls(2)
        .build()
        .unwrap();
    let mut events = Vec::new();

    let options = RunOptions {
        cancel: cancel.clone(),
        ..RunOptions::default()
    };
    let outcome = agent
        .run_with_options("go", options, |event| {
            events.push(event.clone());
        })
        .await
        .unwrap();

    assert_eq!(outcome.status(), RunStatus::Cancelled);
    assert_eq!(started_ids(&events), ["call-1", "call-2"]);
    assert_eq!(result_ids(&events), ["call-2", "call-1"]);
    assert_eq!(
        tool_contents(&outcome),
        ["first".to_owned(), "second".to_owned()]
    );
    assert_eq!(max_active.load(Ordering::SeqCst), 2);
}

struct PanicTool;

#[async_trait]
impl Tool for PanicTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "panic".to_owned(),
            description: "Panics.".to_owned(),
            input_schema: "{}".to_owned(),
        }
    }

    fn effect_class(&self) -> autra_core::EffectClass {
        autra_core::EffectClass::ReadOnly
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        panic!("boom")
    }
}

#[tokio::test]
async fn a_tool_panic_becomes_an_unknown_outcome() {
    let model = tool_model(vec![
        ToolCallRequest {
            call_id: "call-1".to_owned(),
            name: "panic".to_owned(),
            arguments: "{}".to_owned(),
        },
        ToolCallRequest {
            call_id: "call-2".to_owned(),
            name: "panic".to_owned(),
            arguments: "{}".to_owned(),
        },
    ]);
    let agent = Agent::builder("panic")
        .model(model)
        .tool(PanicTool)
        .max_parallel_tool_calls(2)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let history = react_history(outcome.state()).unwrap();
    let panicked: Vec<_> = history
        .iter()
        .filter_map(|item| match item {
            HistoryItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    assert_eq!(panicked.len(), 2);
    assert!(
        panicked
            .iter()
            .all(|tool| tool.is_error && tool.outcome_unknown)
    );
    assert!(panicked[0].content.contains("panicked"));
}

fn result_ids(events: &[RunEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            RunEvent::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_tool_panic_is_caught_in_sequential_mode() {
    let model = tool_model(vec![ToolCallRequest {
        call_id: "call-1".to_owned(),
        name: "panic".to_owned(),
        arguments: "{}".to_owned(),
    }]);
    let agent = Agent::builder("panic-sequential")
        .model(model)
        .tool(PanicTool)
        .max_parallel_tool_calls(1)
        .build()
        .unwrap();

    let outcome = agent.run("go").await.unwrap();

    assert_eq!(outcome.answer(), Some("done"));
    let history = react_history(outcome.state()).unwrap();
    let tool = history
        .iter()
        .find_map(|item| match item {
            HistoryItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .unwrap();
    assert!(tool.is_error);
    assert!(tool.outcome_unknown);
}

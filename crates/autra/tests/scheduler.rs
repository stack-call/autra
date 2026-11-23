use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use autra::{Agent, Dispatcher, SchedulerLimits};
use autra_core::{HistoryItem, ModelCallSpec, ModelOutput, RunId, ToolCallRequest, ToolOutcome};
use autra_execution::ModelError;

struct SlowDispatcher {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[async_trait]
impl Dispatcher for SlowDispatcher {
    async fn call_model(
        &self,
        request: &ModelCallSpec,
        _timeout: Option<Duration>,
    ) -> Result<ModelOutput, ModelError> {
        let saw_tool = request
            .messages
            .iter()
            .any(|item| matches!(item, HistoryItem::Tool(_)));
        if saw_tool {
            return Ok(ModelOutput {
                usage: None,
                text: Some("done".to_owned()),
                tool_calls: Vec::new(),
            });
        }
        Ok(ModelOutput {
            usage: None,
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "slow".to_owned(),
                arguments: "{}".to_owned(),
            }],
        })
    }

    async fn call_tool(
        &self,
        _run_id: &RunId,
        call: &ToolCallRequest,
        _timeout: Option<Duration>,
    ) -> ToolOutcome {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        ToolOutcome {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: "ok".to_owned(),
            is_error: false,
            outcome_unknown: false,
        }
    }
}

#[tokio::test]
async fn two_runs_share_the_global_tool_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let dispatcher = SlowDispatcher {
        active: active.clone(),
        max_active: max_active.clone(),
    };
    let agent = Agent::builder("scheduler")
        .dispatcher(dispatcher)
        .scheduler_limits(SchedulerLimits::new(2, 1, 1))
        .build()
        .unwrap();

    let (first, second) = tokio::join!(agent.run("one"), agent.run("two"));

    assert_eq!(first.unwrap().answer(), Some("done"));
    assert_eq!(second.unwrap().answer(), Some("done"));
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn one_hundred_runs_respect_the_global_tool_limit() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let dispatcher = SlowDispatcher {
        active: active.clone(),
        max_active: max_active.clone(),
    };
    let agent = Arc::new(
        Agent::builder("scheduler-load")
            .dispatcher(dispatcher)
            .scheduler_limits(SchedulerLimits::new(100, 4, 4))
            .build()
            .unwrap(),
    );
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..100 {
        let agent = agent.clone();
        tasks.spawn(async move { agent.run(&format!("run-{index}")).await });
    }

    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().unwrap().answer(), Some("done"));
    }

    assert!(
        max_active.load(Ordering::SeqCst) <= 4,
        "tool concurrency exceeded the global limit"
    );
}

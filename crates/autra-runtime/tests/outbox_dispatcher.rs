use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use autra_core::{
    CancelFlag, EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus, ModelCallSpec,
    ModelOutput, RunId, ToolCallRequest, ToolCallSpec, ToolOutcome,
};
use autra_execution::ModelError;
use autra_runtime::{
    DEFAULT_CLAIM_LEASE, DispatchOptions, DispatchOutcome, Dispatcher, EffectJournal,
    MemoryJournal, OutboxDispatcher,
};

struct StaticDispatcher {
    model_ok: bool,
}

#[async_trait]
impl Dispatcher for StaticDispatcher {
    async fn call_model(
        &self,
        _request: &ModelCallSpec,
        _timeout: Option<Duration>,
    ) -> Result<ModelOutput, ModelError> {
        if self.model_ok {
            Ok(ModelOutput {
                text: Some("model-ok".to_owned()),
                tool_calls: Vec::new(),
            })
        } else {
            Err(ModelError::new("model-boom"))
        }
    }

    async fn call_tool(
        &self,
        _run_id: &RunId,
        call: &ToolCallRequest,
        _timeout: Option<Duration>,
    ) -> ToolOutcome {
        ToolOutcome {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: "tool-ok".to_owned(),
            is_error: false,
            outcome_unknown: false,
        }
    }
}

#[derive(Clone, Default)]
struct CountingDispatcher {
    calls: Arc<AtomicUsize>,
}

impl CountingDispatcher {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Dispatcher for CountingDispatcher {
    async fn call_model(
        &self,
        _request: &ModelCallSpec,
        _timeout: Option<Duration>,
    ) -> Result<ModelOutput, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelOutput {
            text: Some("counted-model".to_owned()),
            tool_calls: Vec::new(),
        })
    }

    async fn call_tool(
        &self,
        _run_id: &RunId,
        call: &ToolCallRequest,
        _timeout: Option<Duration>,
    ) -> ToolOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutcome {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: "counted".to_owned(),
            is_error: false,
            outcome_unknown: false,
        }
    }
}

fn enqueue(
    journal: &MemoryJournal,
    run_id: &RunId,
    intent: EffectIntent,
    kind: EffectKind,
) -> EffectRecord {
    let mut record = EffectRecord::new(EffectId::fresh(), kind, None, None, "digest".to_owned())
        .with_intent_json(serde_json::to_string(&intent).unwrap());
    record.set_status(EffectStatus::Authorized);
    journal.record(run_id, &record).unwrap();
    journal.enqueue_effect(run_id, &record).unwrap();
    record
}

#[tokio::test]
async fn claims_executes_and_completes_a_tool_effect() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-tool");
    let effect = enqueue(
        &journal,
        &run_id,
        EffectIntent::CallTool(ToolCallSpec {
            call: ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            },
        }),
        EffectKind::ToolCall,
    );
    let dispatcher = StaticDispatcher { model_ok: true };

    let results = OutboxDispatcher::run_once_for_run(&journal, &run_id, 10, &dispatcher, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].effect_id, effect.id);
    assert!(matches!(results[0].outcome, DispatchOutcome::Tool(_)));
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::Committed);
    assert_eq!(stored[0].result.as_deref(), Some("tool-ok"));
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn claims_executes_and_completes_a_model_effect() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-model");
    let effect = enqueue(
        &journal,
        &run_id,
        EffectIntent::CallModel(ModelCallSpec {
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
        }),
        EffectKind::ModelCall,
    );
    let dispatcher = StaticDispatcher { model_ok: true };

    OutboxDispatcher::run_once_for_run(&journal, &run_id, 10, &dispatcher, None)
        .await
        .unwrap();

    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].id, effect.id);
    assert_eq!(stored[0].status, EffectStatus::Committed);
    assert_eq!(stored[0].result.as_deref(), Some("model-ok"));
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn a_model_error_completes_the_effect_as_failed() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-fail");
    enqueue(
        &journal,
        &run_id,
        EffectIntent::CallModel(ModelCallSpec {
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
        }),
        EffectKind::ModelCall,
    );
    let dispatcher = StaticDispatcher { model_ok: false };

    OutboxDispatcher::run_once_for_run(&journal, &run_id, 10, &dispatcher, None)
        .await
        .unwrap();

    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::Failed);
    assert!(stored[0].error.as_deref().unwrap().contains("model-boom"));
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_before_dispatch_removes_the_outbox_row() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-cancel");
    let effect = enqueue(
        &journal,
        &run_id,
        EffectIntent::CallTool(ToolCallSpec {
            call: ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            },
        }),
        EffectKind::ToolCall,
    );
    let dispatcher = StaticDispatcher { model_ok: true };
    let cancel = CancelFlag::new();
    cancel.cancel();

    let results = OutboxDispatcher::run_once_for_run_with(
        &journal,
        &run_id,
        DispatchOptions {
            limit: 10,
            max_parallel: 2,
            timeout: None,
            cancel: Some(&cancel),
            lease: DEFAULT_CLAIM_LEASE,
        },
        &dispatcher,
        |_| {},
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0].outcome, DispatchOutcome::Cancelled));
    assert_eq!(results[0].effect_id, effect.id);
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::CancelledBeforeDispatch);
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn a_terminal_record_is_cleaned_up_without_dispatching() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-terminal");
    let effect = enqueue(
        &journal,
        &run_id,
        EffectIntent::CallTool(ToolCallSpec {
            call: ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            },
        }),
        EffectKind::ToolCall,
    );
    let mut stored = journal.load(&run_id).unwrap().remove(0);
    stored.set_status(EffectStatus::Committed);
    stored.record_attempt(
        autra_core::AttemptStatus::Succeeded,
        Some("done".to_owned()),
        None,
    );
    journal.record(&run_id, &stored).unwrap();
    let dispatcher = CountingDispatcher::default();

    let results = OutboxDispatcher::run_once_for_run(&journal, &run_id, 10, &dispatcher, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(
        results[0].outcome,
        DispatchOutcome::AlreadyResolved
    ));
    assert_eq!(results[0].effect_id, effect.id);
    assert_eq!(dispatcher.calls(), 0);
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::Committed);
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn an_in_flight_record_becomes_unknown_on_a_reclaim() {
    let journal = MemoryJournal::new();
    let run_id = RunId::new("run-in-flight");
    enqueue(
        &journal,
        &run_id,
        EffectIntent::CallTool(ToolCallSpec {
            call: ToolCallRequest {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            },
        }),
        EffectKind::ToolCall,
    );
    let mut stored = journal.load(&run_id).unwrap().remove(0);
    stored.set_status(EffectStatus::Dispatched);
    journal.record(&run_id, &stored).unwrap();
    let dispatcher = CountingDispatcher::default();

    let results = OutboxDispatcher::run_once_for_run(&journal, &run_id, 10, &dispatcher, None)
        .await
        .unwrap();

    assert!(matches!(results[0].outcome, DispatchOutcome::Interrupted));
    assert_eq!(dispatcher.calls(), 0);
    let stored = journal.load(&run_id).unwrap();
    assert_eq!(stored[0].status, EffectStatus::OutcomeUnknown);
    assert_eq!(
        stored[0].attempts.last().unwrap().status,
        autra_core::AttemptStatus::OutcomeUnknown
    );
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

#[tokio::test]
async fn run_due_dispatches_rows_across_runs() {
    let journal = MemoryJournal::new();
    for run in ["run-a", "run-b"] {
        enqueue(
            &journal,
            &RunId::new(run),
            EffectIntent::CallTool(ToolCallSpec {
                call: ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                    arguments: "{}".to_owned(),
                },
            }),
            EffectKind::ToolCall,
        );
    }
    let dispatcher = CountingDispatcher::default();

    let results = OutboxDispatcher::run_due(&journal, 10, &dispatcher, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(dispatcher.calls(), 2);
    assert!(
        results
            .iter()
            .all(|result| matches!(result.outcome, DispatchOutcome::Tool(_)))
    );
    assert!(journal.claim_due_effects(10, "check").unwrap().is_empty());
}

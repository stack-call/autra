use std::time::Duration;

use async_trait::async_trait;
use autra_core::{
    EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus, ModelCallSpec, ModelOutput,
    RunId, ToolCallRequest, ToolCallSpec, ToolOutcome,
};
use autra_harness::ModelError;
use autra_runtime::{DispatchOutcome, Dispatcher, EffectJournal, MemoryJournal, OutboxDispatcher};

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

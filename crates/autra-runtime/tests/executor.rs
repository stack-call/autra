use async_trait::async_trait;
use autra_core::{
    EffectId, EffectIntent, EffectKind, EffectRecord, EffectStatus, ModelCallSpec, ModelOutput,
    RunId,
};
use autra_execution::{ModelError, ModelProvider, ToolRegistry};
use autra_runtime::{DispatchOptions, EffectJournal, LocalDispatcher, LocalEngine, MemoryJournal};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
struct Model(Arc<AtomicUsize>);
#[async_trait]
impl ModelProvider for Model {
    async fn complete(&self, _: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ModelOutput {
            text: Some("done".into()),
            tool_calls: vec![],
            usage: None,
        })
    }
}
#[tokio::test]
async fn executor_owns_dependencies_and_outlives_its_engine() {
    let journal = Arc::new(MemoryJournal::new());
    let run = RunId::fresh();
    let calls = Arc::new(AtomicUsize::new(0));
    let intent = EffectIntent::CallModel(ModelCallSpec {
        system: None,
        messages: vec![],
        tools: vec![],
    });
    let mut record = EffectRecord::new(
        EffectId::fresh(),
        EffectKind::ModelCall,
        None,
        None,
        "digest".into(),
    )
    .with_intent_json(serde_json::to_string(&intent).unwrap());
    record.status = EffectStatus::Authorized;
    journal.record(&run, &record).unwrap();
    journal.enqueue_effect(&run, &record).unwrap();
    let engine = LocalEngine::new(
        Arc::new(LocalDispatcher::new(
            Arc::new(Model(calls.clone())),
            ToolRegistry::new(),
        )),
        ToolRegistry::new(),
    )
    .with_journal(journal.clone());
    let executor = engine.effect_executor();
    drop(engine);
    executor
        .execute_pending(
            &run,
            DispatchOptions {
                limit: 1,
                max_parallel: 1,
                timeout: None,
                cancel: None,
                lease: Duration::from_secs(60),
                budget: None,
                child_execution: None,
                delta: None,
                scheduler: None,
                result_inbox: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        journal.load(&run).unwrap()[0].status,
        EffectStatus::Committed
    );
}

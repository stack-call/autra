//! Benchmark-only forwarding decorator. Counts Journal API calls, not SQL statements.
use autra_core::{AcceptedEvent, AgentState, EffectId, EffectRecord, EngineStateDelta, RunId};
use autra_execution::{
    ApprovalStore, CommitAuthority, DurableTransitionStore, EffectJournal, EffectResult,
    JournalError, LeaseRecord, OutboxEntry,
};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
#[derive(Default)]
pub struct Counts {
    total: AtomicU64,
    outbox_peek: AtomicU64,
    pending_results: AtomicU64,
    cancel_request: AtomicU64,
}
#[derive(Clone, Copy, Serialize)]
pub struct Snapshot {
    pub total: u64,
    pub outbox_peek: u64,
    pub pending_results: u64,
    pub cancel_request: u64,
}
impl Snapshot {
    pub fn since(self, before: Self) -> Self {
        Self {
            total: self.total - before.total,
            outbox_peek: self.outbox_peek - before.outbox_peek,
            pending_results: self.pending_results - before.pending_results,
            cancel_request: self.cancel_request - before.cancel_request,
        }
    }
}
impl Counts {
    fn record(&self, method: &str) {
        self.total.fetch_add(1, Ordering::Relaxed);
        let counter = match method {
            "peek_due_effects" => &self.outbox_peek,
            "pending_results" => &self.pending_results,
            "cancel_request" => &self.cancel_request,
            _ => return,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            total: self.total.load(Ordering::Relaxed),
            outbox_peek: self.outbox_peek.load(Ordering::Relaxed),
            pending_results: self.pending_results.load(Ordering::Relaxed),
            cancel_request: self.cancel_request.load(Ordering::Relaxed),
        }
    }
}
pub struct MeteredJournal {
    pub inner: Arc<dyn DurableTransitionStore>,
    pub counts: Arc<Counts>,
}
macro_rules! forward {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)*) => {$(
        #[allow(clippy::too_many_arguments)]
        fn $name(&self, $($arg: $ty),*) -> $result {
            self.counts.record(stringify!($name));
            self.inner.$name($($arg),*)
        }
    )*};
}
impl EffectJournal for MeteredJournal {
    fn supports_result_inbox(&self) -> bool {
        self.inner.supports_result_inbox()
    }
    forward! {
        fn authorize_effect(run: &RunId, effect: &EffectRecord) -> Result<(), JournalError>;
        fn peek_due_effects(run: Option<&RunId>, limit: usize) -> Result<Vec<OutboxEntry>, JournalError>;
        fn claim_effect(effect: &EffectId, claim: &str, lease: Duration) -> Result<Option<OutboxEntry>, JournalError>;
        fn record_result_claimed(run: &RunId, effect: &EffectRecord, claim: &str, result: &EffectResult) -> Result<(), JournalError>;
        fn pending_results(run: &RunId, limit: usize) -> Result<Vec<EffectResult>, JournalError>;
        fn record(run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError>;
        fn load(run_id: &RunId) -> Result<Vec<EffectRecord>, JournalError>;
        fn load_effect(run_id: &RunId, effect_id: &EffectId) -> Result<Option<EffectRecord>, JournalError>;
        fn load_effects(run_id: &RunId, effect_ids: &[EffectId]) -> Result<Vec<EffectRecord>, JournalError>;
        fn record_claimed(run_id: &RunId, effect: &EffectRecord, claim_id: &str) -> Result<(), JournalError>;
        fn renew_claim(effect_id: &EffectId, claim_id: &str, lease: Duration) -> Result<bool, JournalError>;
        fn claim_run(run_id: &RunId, owner_id: &str, lease: Duration) -> Result<bool, JournalError>;
        fn renew_run(run_id: &RunId, owner_id: &str, lease: Duration) -> Result<bool, JournalError>;
        fn release_run(run_id: &RunId, owner_id: &str) -> Result<(), JournalError>;
        fn run_lease(run_id: &RunId) -> Result<Option<LeaseRecord>, JournalError>;
        fn claim_service(service: &str, owner_id: &str, lease: Duration) -> Result<bool, JournalError>;
        fn renew_service(service: &str, owner_id: &str, lease: Duration) -> Result<bool, JournalError>;
        fn release_service(service: &str, owner_id: &str) -> Result<(), JournalError>;
        fn request_cancel(run_id: &RunId, reason: &str) -> Result<(), JournalError>;
        fn cancel_request(run_id: &RunId) -> Result<Option<String>, JournalError>;
        fn clear_cancel(run_id: &RunId) -> Result<(), JournalError>;
        fn save_run(state: &AgentState) -> Result<(), JournalError>;
        fn load_run(run_id: &RunId) -> Result<Option<AgentState>, JournalError>;
        fn list_run_ids() -> Result<Vec<RunId>, JournalError>;
        fn commit_event(run_id: &RunId, event: &AcceptedEvent, state: &AgentState, effects: &[EffectRecord]) -> Result<(), JournalError>;
        fn load_events(run_id: &RunId) -> Result<Vec<AcceptedEvent>, JournalError>;
        fn enqueue_effect(run_id: &RunId, effect: &EffectRecord) -> Result<(), JournalError>;
        fn claim_due_effects(limit: usize, claim_id: &str) -> Result<Vec<OutboxEntry>, JournalError>;
        fn claim_due_effects_for_run(run_id: &RunId, limit: usize, claim_id: &str) -> Result<Vec<OutboxEntry>, JournalError>;
        fn claim_due_effects_with_lease(limit: usize, claim_id: &str, lease: Duration) -> Result<Vec<OutboxEntry>, JournalError>;
        fn claim_due_effects_for_run_with_lease(run_id: &RunId, limit: usize, claim_id: &str, lease: Duration) -> Result<Vec<OutboxEntry>, JournalError>;
        fn complete_outbox(effect_id: &EffectId, claim_id: &str) -> Result<(), JournalError>;
        fn release_outbox(effect_id: &EffectId, claim_id: &str) -> Result<(), JournalError>;
    }
}
impl DurableTransitionStore for MeteredJournal {
    fn journal(&self) -> &dyn EffectJournal {
        self
    }
    fn approval_store(&self) -> &dyn ApprovalStore {
        self.inner.approval_store()
    }
    forward! {
        fn resolve_effect_owned(run: &RunId, effect: &EffectRecord, owner: &str) -> Result<(), JournalError>;
        fn commit_owned_transition(run: &RunId, expected: u64, event: &AcceptedEvent, state: &AgentState, effects: &[EffectRecord], delta: Option<&EngineStateDelta>, authority: &CommitAuthority) -> Result<(), JournalError>;
        fn commit_owned_terminal(run: &RunId, expected: u64, state: &AgentState, authority: &CommitAuthority) -> Result<(), JournalError>;
        fn commit_transition(run_id: &RunId, expected_version: u64, event: &AcceptedEvent, state: &AgentState, new_effects: &[EffectRecord]) -> Result<(), JournalError>;
        fn commit_terminal(run_id: &RunId, expected_version: u64, state: &AgentState) -> Result<(), JournalError>;
        fn commit_transition_with_engine_delta(run_id: &RunId, expected_version: u64, event: &AcceptedEvent, state: &AgentState, new_effects: &[EffectRecord], delta: Option<&EngineStateDelta>) -> Result<(), JournalError>;
    }
}

#[derive(Default)]
pub struct Latencies {
    transition_queue_wait_us: Latency,
    sqlite_writer_wait_us: Latency,
    sqlite_writer_hold_us: Latency,
    renewal_start_lag_us: Latency,
    renewal_end_lag_us: Latency,
}

impl Latencies {
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "transition_queue_wait_us": self.transition_queue_wait_us.snapshot(),
            "sqlite_writer_wait_us": self.sqlite_writer_wait_us.snapshot(),
            "sqlite_writer_hold_us": self.sqlite_writer_hold_us.snapshot(),
            "renewal_start_lag_us": self.renewal_start_lag_us.snapshot(),
            "renewal_end_lag_us": self.renewal_end_lag_us.snapshot(),
        })
    }
}

#[derive(Default)]
struct Latency {
    buckets: [AtomicU64; 32],
    total: AtomicU64,
    max: AtomicU64,
}

impl Latency {
    fn record(&self, micros: u64) {
        let index = (micros.checked_ilog2().unwrap_or(0) as usize).min(self.buckets.len() - 1);
        self.buckets[index].fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(micros, Ordering::Relaxed);
        self.max.fetch_max(micros, Ordering::Relaxed);
    }

    fn snapshot(&self) -> serde_json::Value {
        let counts = self.buckets.each_ref().map(|b| b.load(Ordering::Relaxed));
        let count: u64 = counts.iter().sum();
        let total = self.total.load(Ordering::Relaxed);
        let max = self.max.load(Ordering::Relaxed);
        let p95_upper = counts
            .iter()
            .scan(0, |seen, n| {
                *seen += n;
                Some(*seen)
            })
            .position(|seen| count > 0 && seen >= (count * 95).div_ceil(100))
            .map(|index| ((2u64 << index) - 1).min(max));
        serde_json::json!({"count": count, "mean": (count > 0).then(|| total as f64 / count as f64),
            "p95_upper": p95_upper, "max": (count > 0).then_some(max)})
    }
}

struct ReadLatencies<'a>(&'a Latencies);
impl tracing::field::Visit for ReadLatencies<'_> {
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}

    fn record_u64(&mut self, field: &tracing::field::Field, micros: u64) {
        match field.name() {
            "transition_queue_wait_us" => &self.0.transition_queue_wait_us,
            "sqlite_writer_wait_us" => &self.0.sqlite_writer_wait_us,
            "sqlite_writer_hold_us" => &self.0.sqlite_writer_hold_us,
            "renewal_start_lag_us" => &self.0.renewal_start_lag_us,
            "renewal_end_lag_us" => &self.0.renewal_end_lag_us,
            _ => return,
        }
        .record(micros);
    }
}

struct LatencyLayer(Arc<Latencies>);
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LatencyLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        event.record(&mut ReadLatencies(&self.0));
    }
}

pub fn install(latencies: Arc<Latencies>) {
    use tracing_subscriber::{Layer, prelude::*};
    let layer =
        LatencyLayer(latencies).with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.target() == "autra::runtime_latency"
        }));
    tracing_subscriber::registry().with(layer).init();
}

//! Result collection: turn Effect outcomes into accepted events.

use std::collections::VecDeque;

use autra_core::{AcceptedEvent, EventKind, ModelOutput, ToolOutcome};
use autra_execution::RunEvent;

pub(in crate::engine) fn enqueue(
    queue: &mut VecDeque<AcceptedEvent>,
    seq: &mut u64,
    kind: EventKind,
) {
    *seq += 1;
    queue.push_back(AcceptedEvent { seq: *seq, kind });
}

pub(in crate::engine) fn queue_cancel(
    queue: &mut VecDeque<AcceptedEvent>,
    seq: &mut u64,
    cancel_queued: &mut bool,
) {
    if !*cancel_queued {
        enqueue(queue, seq, EventKind::CancelRequested);
        *cancel_queued = true;
    }
}

pub(in crate::engine) fn emit_tool_result<F: FnMut(&RunEvent)>(
    on_event: &mut F,
    outcome: &ToolOutcome,
) {
    on_event(&RunEvent::ToolResult {
        call_id: outcome.call_id.clone(),
        name: outcome.name.clone(),
        is_error: outcome.is_error,
        outcome_unknown: outcome.outcome_unknown,
        content: outcome.content.clone(),
    });
}

/// Owns the accepted-event queue for one Run. Executors report outcomes; the
/// collector gives them stable order and deduplicates a cancellation request.
pub(super) struct ResultCollector {
    queue: VecDeque<AcceptedEvent>,
    seq: u64,
    cancel_queued: bool,
}

impl ResultCollector {
    pub(super) fn new(queue: VecDeque<AcceptedEvent>) -> Self {
        let seq = queue.iter().map(|event| event.seq).max().unwrap_or(0);
        Self {
            queue,
            seq,
            cancel_queued: false,
        }
    }

    pub(super) fn advance_seq(&mut self, seq: u64) {
        self.seq = self.seq.max(seq);
    }
    pub(super) fn seq(&self) -> u64 {
        self.seq
    }

    pub(super) fn pop_front(&mut self) -> Option<AcceptedEvent> {
        self.queue.pop_front()
    }

    pub(super) fn enqueue(&mut self, kind: EventKind) {
        enqueue(&mut self.queue, &mut self.seq, kind);
    }

    pub(super) fn tool_result(&mut self, outcome: ToolOutcome) {
        self.enqueue(EventKind::ToolResult(outcome));
    }

    pub(super) fn model_result(&mut self, output: ModelOutput) {
        self.enqueue(EventKind::ModelResult(output));
    }

    pub(super) fn cancel(&mut self) {
        queue_cancel(&mut self.queue, &mut self.seq, &mut self.cancel_queued);
    }
}

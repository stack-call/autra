//! Result collection: turn Effect outcomes into accepted events.

use std::collections::VecDeque;

use autra_core::{AcceptedEvent, EventKind, ToolOutcome};
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

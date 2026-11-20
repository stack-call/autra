//! Model delta streaming glue for the coordinator.

use std::future::Future;
use std::sync::Arc;

use autra_execution::RunEvent;

use crate::dispatcher::DeltaSink;

/// Create a model delta sink and the receiver the engine drains. The sink is
/// `Fn`, so a dispatcher can hold it across an await while the run loop keeps
/// consuming deltas.
pub(super) fn delta_channel() -> (DeltaSink, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let sink: DeltaSink = Arc::new(move |text: String| {
        let _ = sender.send(text);
    });
    (sink, receiver)
}

/// Drive a model call while forwarding deltas as `ModelDelta` events. Deltas
/// that arrive just before the call finishes are still delivered in order.
pub(super) async fn await_with_deltas<Fut, T, F>(
    future: Fut,
    deltas: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    on_event: &mut F,
) -> T
where
    Fut: Future<Output = T>,
    F: FnMut(&RunEvent),
{
    tokio::pin!(future);
    let result = loop {
        tokio::select! {
            result = &mut future => break result,
            Some(text) = deltas.recv() => on_event(&RunEvent::ModelDelta { text }),
        }
    };
    while let Ok(text) = deltas.try_recv() {
        on_event(&RunEvent::ModelDelta { text });
    }
    result
}

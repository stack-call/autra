//! Model delta streaming glue for the coordinator.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

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

const DELTA_BATCH_BYTES: usize = 512;
const DELTA_FLUSH_MS: u64 = 8;

/// Drive a model call while forwarding deltas as `ModelDelta` events. Small
/// bursts are coalesced into one event, so a token stream does not create one
/// event (and one broadcast clone) per token. Order is preserved.
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
    let mut buffer = String::new();
    let mut flush_at: Option<tokio::time::Instant> = None;
    let result = loop {
        let flush = async {
            match flush_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            result = &mut future => break result,
            Some(text) = deltas.recv() => {
                buffer.push_str(&text);
                if buffer.len() >= DELTA_BATCH_BYTES {
                    flush_delta(&mut buffer, on_event);
                    flush_at = None;
                } else if flush_at.is_none() {
                    flush_at = Some(
                        tokio::time::Instant::now() + Duration::from_millis(DELTA_FLUSH_MS),
                    );
                }
            }
            _ = flush => {
                flush_delta(&mut buffer, on_event);
                flush_at = None;
            }
        }
    };
    while let Ok(text) = deltas.try_recv() {
        buffer.push_str(&text);
        if buffer.len() >= DELTA_BATCH_BYTES {
            flush_delta(&mut buffer, on_event);
        }
    }
    flush_delta(&mut buffer, on_event);
    result
}

fn flush_delta<F: FnMut(&RunEvent)>(buffer: &mut String, on_event: &mut F) {
    if buffer.is_empty() {
        return;
    }
    let text = std::mem::take(buffer);
    on_event(&RunEvent::ModelDelta { text });
}

//! In-process Run snapshot and event subscription hub.

use std::collections::BTreeMap;
use std::sync::Mutex;

use autra_core::RunId;
use autra_execution::{RunEvent, RunSnapshot};
use tokio::sync::broadcast;

const HUB_CAPACITY: usize = 256;

#[derive(Default)]
pub struct RunHub {
    entries: Mutex<BTreeMap<RunId, HubEntry>>,
}

struct HubEntry {
    snapshot: RunSnapshot,
    sender: Option<broadcast::Sender<RunEvent>>,
}

impl RunHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a live Run and return its event channel. Subscribers can
    /// subscribe after this call and before `finish`.
    pub fn start(&self, snapshot: RunSnapshot) -> broadcast::Sender<RunEvent> {
        let (sender, _) = broadcast::channel(HUB_CAPACITY);
        let mut entries = self.entries.lock().expect("hub lock poisoned");
        entries.insert(
            snapshot.run_id.clone(),
            HubEntry {
                snapshot,
                sender: Some(sender.clone()),
            },
        );
        sender
    }

    pub fn publish(&self, run_id: &RunId, event: &RunEvent) {
        let entries = self.entries.lock().expect("hub lock poisoned");
        if let Some(entry) = entries.get(run_id) {
            if let Some(sender) = &entry.sender {
                let _ = sender.send(event.clone());
            }
        }
    }

    pub fn update(&self, snapshot: RunSnapshot) {
        let mut entries = self.entries.lock().expect("hub lock poisoned");
        if let Some(entry) = entries.get_mut(&snapshot.run_id) {
            entry.snapshot = snapshot;
        }
    }

    /// Mark a Run finished; live subscribers receive the remaining events and
    /// then a closed channel.
    pub fn finish(&self, snapshot: RunSnapshot) {
        let mut entries = self.entries.lock().expect("hub lock poisoned");
        if let Some(entry) = entries.get_mut(&snapshot.run_id) {
            entry.snapshot = snapshot;
            entry.sender = None;
        }
    }

    pub fn snapshot(&self, run_id: &RunId) -> Option<RunSnapshot> {
        self.entries
            .lock()
            .expect("hub lock poisoned")
            .get(run_id)
            .map(|entry| entry.snapshot.clone())
    }

    pub fn runs(&self) -> Vec<RunSnapshot> {
        self.entries
            .lock()
            .expect("hub lock poisoned")
            .values()
            .map(|entry| entry.snapshot.clone())
            .collect()
    }

    pub fn subscribe(&self, run_id: &RunId) -> Option<RunSubscription> {
        let entries = self.entries.lock().expect("hub lock poisoned");
        let entry = entries.get(run_id)?;
        Some(RunSubscription {
            snapshot: entry.snapshot.clone(),
            receiver: entry.sender.as_ref().map(|sender| sender.subscribe()),
        })
    }
}

pub struct RunSubscription {
    pub snapshot: RunSnapshot,
    receiver: Option<broadcast::Receiver<RunEvent>>,
}

impl RunSubscription {
    /// Receive the next event. Returns `None` when the Run has finished or the
    /// subscription had no live sender.
    pub async fn recv(&mut self) -> Option<RunEvent> {
        let receiver = self.receiver.as_mut()?;
        loop {
            match receiver.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

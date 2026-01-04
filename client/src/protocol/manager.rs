use crate::protocol::messages::MqttResponseMessage;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Sender;
use tracing::debug;

pub(crate) struct TimedRequest {
    ts: Instant,
    sender: Sender<MqttResponseMessage>,
}

pub(crate) struct RequestManager {
    pending: DashMap<u32, TimedRequest>,
    timeout: u64,
    running: AtomicBool,
}

impl Default for RequestManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestManager {
    pub fn new() -> Self {
        Self {
            pending: DashMap::new(),
            timeout: 10,
            running: AtomicBool::new(false),
        }
    }

    pub fn start(&self) {
        self.running.store(true, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn add_request(&self, id: u32) -> oneshot::Receiver<MqttResponseMessage> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(
            id,
            TimedRequest {
                sender: tx,
                ts: Instant::now(),
            },
        );
        rx
    }

    pub fn remove_pending_requests(&self) {
        let to_remove: Vec<u32> = self
            .pending
            .iter()
            .filter(|i| i.value().ts.elapsed().as_secs() > self.timeout)
            .map(|i| *i.key())
            .collect();
        for id in to_remove {
            debug!("Removing timed out request {}", id);
            self.pending.remove(&id);
        }
    }

    pub fn complete_request(&self, response: &MqttResponseMessage) -> bool {
        debug!("Complete request: {response:?}");
        if let Some(seq_id) = response.seq_id
            && let Some((_, sender)) = self.pending.remove(&seq_id)
        {
            debug!("Sending completed message for request: {seq_id}");
            sender.sender.send(response.clone()).is_ok()
        } else {
            false
        }
    }
}

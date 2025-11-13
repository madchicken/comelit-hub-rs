use crate::protocol::messages::{MqttResponseMessage};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::oneshot::Sender;
use tokio::sync::oneshot;

pub(crate) struct TimedRequest {
    ts: Instant,
    sender: Sender<MqttResponseMessage>,
}

pub(crate) struct RequestManager {
    pending: DashMap<u32, TimedRequest>,
    timeout: u64,
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
        }
    }

    pub async fn add_request(&self, id: u32) -> oneshot::Receiver<MqttResponseMessage> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, TimedRequest { sender: tx, ts: Instant::now() });
        rx
    }

    pub fn remove_pending_requests(&self) {
        self.pending.iter().filter(|i| i.value().ts.elapsed().as_secs() > self.timeout).for_each(|i| {
            self.pending.remove(i.key());
        });
    }

    pub async fn complete_request(&self, response: &MqttResponseMessage) -> bool {
        if let Some((_, sender)) = self.pending.remove(&response.seq_id.unwrap()) {
            sender.sender.send(response.clone()).is_ok()
        } else {
            false
        }
    }
}
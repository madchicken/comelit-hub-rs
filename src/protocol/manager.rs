use crate::protocol::messages::{MqttMessage, MqttResponseMessage};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::oneshot::Sender;
use tokio::sync::oneshot;

pub struct RequestManager {
    pub pending: Arc<DashMap<u32, Sender<MqttResponseMessage>>>,
}

impl Default for RequestManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestManager {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(DashMap::new()),
        }
    }

    pub async fn add_request(&self, id: u32) -> oneshot::Receiver<MqttResponseMessage> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        rx
    }

    pub async fn remove_request(&self, id: u32) {
        self.pending.remove(&id);
    }

    pub async fn complete_request(&self, response: &MqttResponseMessage) -> bool {
        if let Some((_, sender)) = self.pending.remove(&response.seq_id) {
            sender.send(response.clone()).is_ok()
        } else {
            false
        }
    }
}
use crate::protocol::messages::{MqttResponseMessage};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::oneshot::Sender;
use tokio::sync::oneshot;
use crate::protocol::out_data_messages::HomeDeviceData;

pub(crate) struct TimedRequest {
    ts: Instant,
    sender: Sender<MqttResponseMessage>,
}

pub(crate) struct RequestManager {
    pending: Arc<DashMap<u32, TimedRequest>>,
    timeout: u64,
    index: Option<DashMap<String, HomeDeviceData>>,
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
            timeout: 10,
            index: None,
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

    pub fn set_index(&mut self, index: DashMap<String, HomeDeviceData>) {
        self.index = Some(index);
    }

    pub fn update_index(&self, key: String, value: &HomeDeviceData) {
        if let Some(index) = &self.index {
            index.insert(key, value.clone());
        }
    }
    
    pub fn get_index(&self) -> Option<&DashMap<String, HomeDeviceData>> {
        self.index.as_ref()
    }
}
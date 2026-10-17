use std::io;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tracing::info;

pub struct UdpStreamWrapper {
    stream: Arc<UdpSocket>,
}

impl UdpStreamWrapper {
    pub async fn new(ip: &str, port: u16) -> Result<Self, io::Error> {
        let remote_addr = format!("{}:{}", ip, port);
        let socket = UdpSocket::bind("0.0.0.0:0").await?;

        socket.connect(&remote_addr).await?;

        info!("Connected to remote UDP {}", remote_addr);

        Ok(Self {
            stream: Arc::new(socket),
        })
    }

    /// The socket, shared with the video unit task.
    pub fn socket(&self) -> Arc<UdpSocket> {
        self.stream.clone()
    }
}

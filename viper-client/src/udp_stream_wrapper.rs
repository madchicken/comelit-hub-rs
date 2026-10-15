use std::io;

use tokio::net::UdpSocket;
use tracing::info;

pub struct UdpStreamWrapper {
    stream: UdpSocket,
}

impl UdpStreamWrapper {
    pub async fn new(ip: &str, port: u16) -> Result<Self, io::Error> {
        let remote_addr = format!("{}:{}", ip, port);
        let socket = UdpSocket::bind("0.0.0.0:0").await?;

        socket.connect(&remote_addr).await?;

        info!("Connected to remote UDP {}", remote_addr);

        Ok(Self { stream: socket })
    }

    pub fn socket(&self) -> &UdpSocket {
        &self.stream
    }
}

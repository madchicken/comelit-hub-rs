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

        info!("Sending initial handshake packets...");
        Ok(Self { stream: socket })
    }

    pub async fn send(&self, data: &[u8]) -> Result<(), io::Error> {
        self.stream.send(data).await?;
        Ok(())
    }

    pub async fn recv(&self, buf: &mut [u8]) -> Result<usize, io::Error> {
        self.stream.recv(buf).await
    }
}

use std::io;

use tokio::net::UdpSocket;
use tracing::info;

use crate::helper::Helper;

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

    pub async fn write(&self, data: &[u8]) -> Result<usize, io::Error> {
        Helper::print_buffer(data);
        self.stream.send(data).await
    }

    pub async fn read(&self, buf: &mut [u8]) -> Result<usize, io::Error> {
        tokio::time::timeout(std::time::Duration::from_millis(100), self.stream.recv(buf))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "recv timeout"))?
    }
}

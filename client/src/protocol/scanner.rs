use std::io;
use std::net::UdpSocket;
use std::time::Duration;
use tracing::{debug, error, info};

const MAX_DATAGRAM_SIZE: usize = 65_507;

fn to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .to_string()
}

fn to_hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ComelitHUB {
    mac_address: String,
    hw_id: String,
    app_id: String,
    app_version: String,
    system_id: String,
    description: String,
    model_id: String,
    address: Option<String>,
}

#[allow(dead_code)]
impl ComelitHUB {
    pub fn mac_address(&self) -> &str {
        &self.mac_address
    }

    pub fn hw_id(&self) -> &str {
        &self.hw_id
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn app_version(&self) -> &str {
        &self.app_version
    }

    pub fn system_id(&self) -> &str {
        &self.system_id
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    pub fn model(&self) -> &str {
        match self.model_id.as_str() {
            "Extd" => "1456 - Gateway",
            "ExtS" => "1456S - Gateway",
            "MSVF" => "6741W - Mini SBC/ViP/Extender handsfree",
            "MSVU" => "6741W - Mini SBC/ViP/Extender handsfree",
            "MnWi" => "6742W - Mini ViP handsfree Wifi",
            "MxWi" => "6842W - Maxi ViP 7'' Wifi",
            "Vist" => "Visto - Wifi ViP",
            "HSrv" => "Home server",
            &_ => "Unknown",
        }
    }

    pub fn with_address(mut self, address: String) -> Self {
        self.address = Some(address);
        self
    }
}

impl From<&[u8]> for ComelitHUB {
    fn from(msg: &[u8]) -> Self {
        ComelitHUB {
            mac_address: to_hex_string(&msg[14..20]),
            hw_id: to_string(&msg[20..24]),
            app_id: to_string(&msg[24..28]),
            app_version: to_string(&msg[32..112]),
            system_id: to_string(&msg[112..116]),
            description: to_string(&msg[116..152]),
            model_id: to_string(&msg[156..160]),
            address: None,
        }
    }
}

pub const SCAN_PORT: &str = "24199";

pub struct Scanner;

impl Scanner {
    pub async fn scan(timeout: Option<Duration>) -> Result<Vec<ComelitHUB>, std::io::Error> {
        let socket = UdpSocket::bind("0.0.0.0:34254")?;

        // Set the read timeout to 1 second
        socket.set_read_timeout(timeout)?;
        socket.set_broadcast(true)?;

        let buf: Vec<u8> = vec![b'S', b'C', b'A', b'N', 0, 0, 0, 0, 0, 0xff, 0xff, 0xff];
        socket.send_to(&buf, format!("255.255.255.255:{SCAN_PORT}"))?;

        let mut data = vec![0u8; MAX_DATAGRAM_SIZE];
        let mut result: Vec<ComelitHUB> = Vec::new();
        loop {
            match socket.recv_from(&mut data) {
                Ok((len, source)) => {
                    debug!("Received {} bytes from {}", len, source);
                    let response = String::from_utf8_lossy(&data[..len]);
                    if response.starts_with("here") {
                        let buf: Vec<u8> = vec![b'I', b'N', b'F', b'O', 0, 0, 0, 0, 0, 0, 0, 0];
                        socket.send_to(&buf, source)?;
                        continue;
                    } else {
                        let comelit_hub =
                            ComelitHUB::from(&data[..len]).with_address(source.ip().to_string());
                        info!("Comelit HUB found: {:?}", comelit_hub);
                        result.push(comelit_hub);
                        continue;
                    }
                }
                Err(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    info!("No message received in 2 seconds, closing connection.");
                    break;
                }
                Err(e) => {
                    error!("Error receiving UDP packet: {}", e);
                    return Err(e);
                }
            }
        }
        Ok(result)
    }

    pub async fn scan_address(
        address: &str,
        timeout: Option<Duration>,
    ) -> Result<Option<ComelitHUB>, io::Error> {
        let socket = UdpSocket::bind("0.0.0.0:34254")?;

        // Set the read timeout to 1 second
        socket.set_read_timeout(timeout)?;
        let buf: Vec<u8> = vec![b'I', b'N', b'F', b'O', 0, 0, 0, 0, 0, 0, 0, 0];
        socket.send_to(&buf, format!("{address}:{SCAN_PORT}"))?;
        let mut data = vec![0u8; MAX_DATAGRAM_SIZE];
        match socket.recv_from(&mut data) {
            Ok((len, source)) => {
                let comelit_hub =
                    ComelitHUB::from(&data[..len]).with_address(source.ip().to_string());
                info!("Comelit HUB found: {:?}", comelit_hub);
                Ok(Some(comelit_hub))
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                info!("No message received in 2 seconds, closing connection.");
                Ok(None)
            }
            Err(e) => {
                error!("Error receiving UDP packet: {}", e);
                Err(e)
            }
        }
    }
}

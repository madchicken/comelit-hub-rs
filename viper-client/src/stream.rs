use std::fs::File;
use std::io::{self, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::command::Command;
use crate::command_response::ConfigurationResponse;
use crate::ctpp_channel::CTPPChannel;
use crate::helper::Helper;
use crate::stream_wrapper::StreamWrapper;

const RTP_VIDEO_SUBCHANNEL: [u8; 2] = [0x7a, 0x51];
const RTP_AUDIO_SUBCHANNEL: [u8; 2] = [0x79, 0x51];
const CONTROL_SUBCHANNEL: [u8; 2] = [0x78, 0x51];
const SOLO_UDP_SUBCHANNEL: [u8; 2] = [0x7b, 0x51];
const SOLO_AUDIO_SUBCHANNEL: [u8; 2] = [0x7c, 0x51];
const SOLO_VIDEO_SUBCHANNEL: [u8; 2] = [0x7d, 0x51];
const GIGANTE_CONTROL_SUBCHANNEL: [u8; 2] = [0x48, 0x75];
const GIGANTE_VIDEO_SUBCHANNEL: [u8; 2] = [0x49, 0x75];

const BOOTSTRAP_1: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x78, 0x51, 0x00, 0x00, 0x6b, 0x12, 0x00, 0xfe, 0x00, 0x80,
];
const BOOTSTRAP_2: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x78, 0x51, 0x00, 0x00, 0x6b, 0x12, 0x00, 0xff, 0x00, 0x80,
];
const BOOTSTRAP_3: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x78, 0x51, 0x00, 0x00, 0x6b, 0x12, 0x00, 0x00, 0x01, 0x80,
];
const SOLO_BOOTSTRAP_1: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x7b, 0x51, 0x00, 0x00, 0x6b, 0x12, 0x00, 0x2a, 0x00, 0x80,
];
const SOLO_BOOTSTRAP_2: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x7b, 0x51, 0x00, 0x00, 0x6b, 0x12, 0x00, 0x2b, 0x00, 0x80,
];
const SOLO_BOOTSTRAP_3: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x7b, 0x51, 0x00, 0x00, 0x6b, 0x12, 0x00, 0x2c, 0x01, 0x80,
];
const GIGANTE_BOOTSTRAP_1: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x48, 0x75, 0x00, 0x00, 0x3a, 0x12, 0x00, 0x67, 0x00, 0x80,
];
const GIGANTE_BOOTSTRAP_2: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x48, 0x75, 0x00, 0x00, 0x3a, 0x12, 0x00, 0x68, 0x00, 0x80,
];
const GIGANTE_BOOTSTRAP_3: [u8; 14] = [
    0x00, 0x06, 0x06, 0x00, 0x48, 0x75, 0x00, 0x00, 0x3a, 0x12, 0x00, 0x69, 0x01, 0x80,
];

#[derive(Debug, Default)]
pub struct StreamStats {
    pub video_packets: usize,
    pub audio_packets: usize,
    pub control_packets: usize,
    pub bytes_written: usize,
}

pub fn record_h264_stream<P: AsRef<Path>>(
    stream: &mut StreamWrapper,
    config: &ConfigurationResponse,
    output_path: P,
    duration: Duration,
) -> io::Result<StreamStats> {
    let local_addr = format!("0.0.0.0:{}", config.viper_server.local_udp_port);
    let remote_addr = stream_socket_addr(config)?;

    let socket = UdpSocket::bind(local_addr)?;
    socket.connect(remote_addr)?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    socket.set_write_timeout(Some(Duration::from_millis(500)))?;

    start_media_session(stream, config, &socket)?;

    let mut stats = StreamStats::default();
    let mut out = File::create(output_path)?;
    let mut current_fu = Vec::new();
    let mut buf = [0u8; 4096];
    let start = Instant::now();

    while start.elapsed() < duration {
        match socket.recv(&mut buf) {
            Ok(size) => {
                if size < 8 {
                    continue;
                }

                let subchannel = &buf[4..6];

                if subchannel == RTP_VIDEO_SUBCHANNEL
                    || subchannel == SOLO_VIDEO_SUBCHANNEL
                    || subchannel == GIGANTE_VIDEO_SUBCHANNEL
                {
                    stats.video_packets += 1;
                    let payload = &buf[8..size];
                    if let Some(written) =
                        write_h264_payload(payload, &mut out, &mut current_fu)?
                    {
                        stats.bytes_written += written;
                    }
                } else if subchannel == RTP_AUDIO_SUBCHANNEL || subchannel == SOLO_AUDIO_SUBCHANNEL {
                    stats.audio_packets += 1;
                } else if subchannel == CONTROL_SUBCHANNEL
                    || subchannel == SOLO_UDP_SUBCHANNEL
                    || subchannel == GIGANTE_CONTROL_SUBCHANNEL
                {
                    stats.control_packets += 1;
                }
            }
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                if start.elapsed() < Duration::from_secs(2) {
                    send_bootstrap(&socket)?;
                    send_solo_bootstrap(&socket)?;
                    send_gigante_bootstrap(&socket)?;
                }
            }
            Err(err) => return Err(err),
        }
    }

    if !current_fu.is_empty() {
        out.write_all(&[0, 0, 0, 1])?;
        out.write_all(&current_fu)?;
        stats.bytes_written += 4 + current_fu.len();
    }

    Ok(stats)
}

fn start_media_session(
    stream: &mut StreamWrapper,
    config: &ConfigurationResponse,
    socket: &UdpSocket,
) -> io::Result<()> {
    let apartment = apartment_endpoint(config);
    let addr = config.vip.apt_address.clone();
    let control = Helper::control();
    let mut ctpp = CTPPChannel::new(&control);
    let link_candidates = link_candidates(config, &apartment, &addr);

    eprintln!(
        "call-candidates: apartment={apartment} addr={addr} variants={:?}",
        link_candidates
    );

    let cspb_open = stream.execute(&Command::channel(&String::from("CSPB"), &control, None))?;
    log_tcp_payload("cspb-open", &cspb_open);

    let ctpp_open = stream.execute(&ctpp.open(&apartment))?;
    log_tcp_payload("ctpp-open", &ctpp_open);

    let _ = stream.write(&ctpp.connect_hs(&apartment, &addr))?;
    let mut saw_handshake = false;
    for attempt in 0..4 {
        match stream.read() {
            Ok(payload) => {
                log_tcp_payload(&format!("ctpp-handshake-{attempt}"), &payload);
                if payload.len() >= 6 && ctpp.confirm_handshake(&payload[..6]) {
                    saw_handshake = true;
                    break;
                }
            }
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(err) => return Err(err),
        }
    }

    if !saw_handshake {
        eprintln!("ctpp-handshake: no confirm from device");
    }

    let _ = stream.write(&ctpp.ack(0x00, &apartment, &addr))?;
    thread::sleep(Duration::from_millis(20));
    drain_tcp(stream, "ctpp-ack-00");

    let _ = stream.write(&ctpp.ack(0x20, &apartment, &addr))?;
    thread::sleep(Duration::from_millis(20));
    drain_tcp(stream, "ctpp-ack-20");

    for (label, left, right) in link_candidates {
        let _ = stream.write(&ctpp.link_actuators(&left, &right))?;
        thread::sleep(Duration::from_millis(50));
        drain_tcp(stream, &label);
    }

    send_gigante_bootstrap(socket)?;
    send_solo_bootstrap(socket)?;
    send_bootstrap(socket)?;
    thread::sleep(Duration::from_millis(100));
    drain_tcp(stream, "gigante-bootstrap");

    Ok(())
}

fn stream_socket_addr(config: &ConfigurationResponse) -> io::Result<SocketAddr> {
    let host = if config.viper_server.remote_address.trim().is_empty() {
        &config.viper_server.local_address
    } else {
        &config.viper_server.remote_address
    };

    let port = if config.viper_server.remote_udp_port == 0 {
        config.viper_server.local_udp_port
    } else {
        config.viper_server.remote_udp_port
    };

    format!("{host}:{port}").parse().map_err(io::Error::other)
}

fn send_bootstrap(socket: &UdpSocket) -> io::Result<()> {
    socket.send(&BOOTSTRAP_1)?;
    socket.send(&BOOTSTRAP_2)?;
    socket.send(&BOOTSTRAP_3)?;
    Ok(())
}

fn send_solo_bootstrap(socket: &UdpSocket) -> io::Result<()> {
    socket.send(&SOLO_BOOTSTRAP_1)?;
    socket.send(&SOLO_BOOTSTRAP_2)?;
    socket.send(&SOLO_BOOTSTRAP_3)?;
    Ok(())
}

fn send_gigante_bootstrap(socket: &UdpSocket) -> io::Result<()> {
    socket.send(&GIGANTE_BOOTSTRAP_1)?;
    socket.send(&GIGANTE_BOOTSTRAP_2)?;
    socket.send(&GIGANTE_BOOTSTRAP_3)?;
    Ok(())
}

fn apartment_endpoint(config: &ConfigurationResponse) -> String {
    format!("{}{}", config.vip.apt_address, config.vip.apt_subaddress)
}

fn peer_endpoint(config: &ConfigurationResponse) -> String {
    config
        .vip
        .user_parameters
        .opendoor_address_book
        .first()
        .map(|entry| entry.apt_address.clone())
        .unwrap_or_else(|| "00000100".to_string())
}

fn link_candidates(
    config: &ConfigurationResponse,
    apartment: &str,
    addr: &str,
) -> Vec<(String, String, String)> {
    let mut peers = Vec::new();

    for action in &config.vip.user_parameters.opendoor_actions {
        if action.action == "peer" {
            peers.push(action.apt_address.clone());
        }
    }

    peers.push(peer_endpoint(config));
    peers.push(String::new());
    peers.push(addr.to_string());
    peers.push(apartment.to_string());

    peers.sort();
    peers.dedup();

    let mut out = Vec::new();
    for peer in peers {
        if peer != apartment {
            out.push((
                format!("ctpp-link-{}-apartment", display_endpoint(&peer)),
                peer.clone(),
                apartment.to_string(),
            ));
            out.push((
                format!("ctpp-link-apartment-{}", display_endpoint(&peer)),
                apartment.to_string(),
                peer.clone(),
            ));
        }

        if peer != addr {
            out.push((
                format!("ctpp-link-{}-addr", display_endpoint(&peer)),
                peer.clone(),
                addr.to_string(),
            ));
            out.push((
                format!("ctpp-link-addr-{}", display_endpoint(&peer)),
                addr.to_string(),
                peer.clone(),
            ));
        }
    }

    out
}

fn display_endpoint(value: &str) -> &str {
    if value.is_empty() { "empty" } else { value }
}

fn put_str(buf: &mut [u8], offset: usize, value: &str) {
    let bytes = value.as_bytes();
    let max = buf.len().saturating_sub(offset);
    let copy_len = bytes.len().min(max.saturating_sub(1));
    buf[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
    if offset + copy_len < buf.len() {
        buf[offset + copy_len] = 0x00;
    }
}

fn drain_tcp(stream: &mut StreamWrapper, label: &str) {
    loop {
        match stream.read() {
            Ok(payload) => log_tcp_payload(label, &payload),
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(err) => {
                eprintln!("{label}: read-error={err}");
                break;
            }
        }
    }
}

fn log_tcp_payload(label: &str, payload: &[u8]) {
    eprintln!("{label}: {}", hex(payload));
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn write_h264_payload(
    payload: &[u8],
    out: &mut File,
    current_fu: &mut Vec<u8>,
) -> io::Result<Option<usize>> {
    if payload.len() < 20 {
        return Ok(None);
    }

    let rtp_payload = &payload[12..];
    if rtp_payload.len() < 2 {
        return Ok(None);
    }

    let nal_type = rtp_payload[0] & 0x1f;

    if nal_type == 28 {
        let fu_header = rtp_payload[1];
        let start_bit = (fu_header >> 7) & 0x01;
        let end_bit = (fu_header >> 6) & 0x01;
        let fragment_type = fu_header & 0x1f;

        if start_bit == 1 {
            current_fu.clear();
            current_fu.push((rtp_payload[0] & 0xe0) | fragment_type);
        }

        current_fu.extend_from_slice(&rtp_payload[2..]);

        if end_bit == 1 && !current_fu.is_empty() {
            out.write_all(&[0, 0, 0, 1])?;
            out.write_all(current_fu)?;
            let written = 4 + current_fu.len();
            current_fu.clear();
            return Ok(Some(written));
        }

        return Ok(None);
    }

    out.write_all(&[0, 0, 0, 1])?;
    out.write_all(rtp_payload)?;
    Ok(Some(4 + rtp_payload.len()))
}

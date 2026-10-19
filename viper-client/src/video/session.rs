//! One-shot video recording for the CLI, on top of a [`ViperUnit`]:
//!
//! * [`CallMode::Watch`] ("auto-ignition"): call the external unit and record
//!   its stream. The unit refuses the call with USER_BUSY while it is ringing.
//! * [`CallMode::Listen`]: register as a subunit of the apartment, wait for
//!   someone to ring and record the early video of the call without
//!   answering it; the other units keep ringing.
//!
//! The recording stops when the call or the media ends, after `duration`
//! (when listening it includes the wait for a call) or when `stop` resolves.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{info, warn};

use super::ctp::{LogAddr, VideoParams};
use super::media::{RtpPacket, StreamRecorder};
use super::unit::{MediaKind, MediaPacket, UnitConfig, UnitEvent, ViperUnit};
use crate::ViperError;

#[derive(Debug, Clone)]
pub struct VideoOptions {
    /// Output path without extension; the extension follows the codec
    /// (`.h264`, `.m4v`, `.alaw`, `.ulaw`).
    pub output: PathBuf,
    /// Stop after this long (when listening: including the wait for a call).
    pub duration: Duration,
    /// Also request the external unit audio.
    pub audio: bool,
    pub video: VideoParams,
}

#[derive(Debug, Default)]
pub struct VideoStats {
    pub udp_lane_open: bool,
    /// Who called us, when listening.
    pub caller: Option<String>,
    pub video_file: Option<PathBuf>,
    pub video_packets: usize,
    pub video_lost: usize,
    pub audio_file: Option<PathBuf>,
    pub audio_packets: usize,
}

pub enum CallMode {
    /// Call the external unit (auto-ignition).
    Watch { external_unit: LogAddr },
    /// Register as a subunit of the apartment and wait for an incoming call,
    /// which we stop watching after `ring_timeout`.
    Listen {
        apartment: LogAddr,
        ttl_secs: u16,
        ring_timeout: Duration,
    },
}

/// `src` is us (apt address + subaddress).
pub struct CallTarget {
    pub src: LogAddr,
    pub mode: CallMode,
}

pub async fn run(
    tcp: TcpStream,
    udp: Arc<UdpSocket>,
    first_channel: u16,
    target: CallTarget,
    options: VideoOptions,
    stop: impl Future<Output = ()>,
) -> Result<VideoStats, ViperError> {
    let listening = matches!(target.mode, CallMode::Listen { .. });
    let config = match target.mode {
        CallMode::Watch { external_unit } => UnitConfig {
            src: target.src,
            apartment: None,
            registration_ttl_secs: 0,
            entrance: Some(external_unit),
            ring_timeout: Duration::ZERO,
            video: options.video,
            keep_watching: false,
        },
        CallMode::Listen {
            apartment,
            ttl_secs,
            ring_timeout,
        } => UnitConfig {
            src: target.src,
            apartment: Some(apartment),
            registration_ttl_secs: ttl_secs,
            entrance: None,
            ring_timeout,
            video: options.video,
            keep_watching: false,
        },
    };
    let (unit, mut events, task) = ViperUnit::spawn(tcp, udp, first_channel, config);
    let mut recording = Recording::new(&options);
    let mut stats = VideoStats::default();

    let result: Result<(), ViperError> = async {
        tokio::pin!(stop);
        let deadline = tokio::time::sleep_until(Instant::now() + options.duration);
        tokio::pin!(deadline);
        let mut packets: Option<mpsc::Receiver<MediaPacket>> = None;
        if !listening {
            info!("Calling the external unit");
            packets = Some(unit.start_media(options.audio).await?);
        } else {
            info!("Registering, then waiting for a call");
        }
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(UnitEvent::UdpLaneOpen) => stats.udp_lane_open = true,
                    Some(UnitEvent::Registered) => info!("Registered, waiting for a call"),
                    Some(UnitEvent::Ringing { caller }) => {
                        info!("Incoming call from {caller:?}");
                        stats.caller = Some(format!("{caller:?}"));
                        packets = Some(unit.start_media(options.audio).await?);
                    }
                    Some(UnitEvent::RingEnded) => {
                        info!("Call released");
                        return Ok(());
                    }
                    Some(UnitEvent::MediaEnded { reason }) => {
                        info!("Media ended: {reason}");
                        return Ok(());
                    }
                    None => return Ok(()),
                },
                packet = recv(&mut packets) => match packet {
                    Some(packet) => recording.push(&packet),
                    None => packets = None,
                },
                _ = &mut deadline => {
                    info!("Recording time elapsed");
                    return Ok(());
                }
                _ = &mut stop => {
                    info!("Stop requested");
                    return Ok(());
                }
            }
        }
    }
    .await;

    let _ = unit.shutdown().await;
    let unit_result = task
        .await
        .map_err(|e| ViperError::Generic(format!("Video unit panicked: {e}")))?;
    recording.finish(&mut stats);
    result?;
    unit_result?;
    Ok(stats)
}

async fn recv(packets: &mut Option<mpsc::Receiver<MediaPacket>>) -> Option<MediaPacket> {
    match packets {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

struct Recording {
    video: StreamRecorder,
    audio: StreamRecorder,
}

impl Recording {
    fn new(options: &VideoOptions) -> Recording {
        Recording {
            video: StreamRecorder::new(options.output.clone()),
            audio: StreamRecorder::new(with_suffix(&options.output, "-audio")),
        }
    }

    fn push(&mut self, packet: &MediaPacket) {
        let Some(rtp) = RtpPacket::parse(&packet.rtp) else {
            return;
        };
        let recorder = match packet.kind {
            MediaKind::Video => &mut self.video,
            MediaKind::Audio => &mut self.audio,
        };
        if recorder.packets == 0 {
            info!(
                "First {:?} RTP packet, payload type {}",
                packet.kind, rtp.payload_type
            );
        }
        if let Err(e) = recorder.push(&rtp) {
            warn!("Cannot record RTP packet: {e}");
        }
    }

    fn finish(&mut self, stats: &mut VideoStats) {
        for recorder in [&mut self.video, &mut self.audio] {
            if let Err(e) = recorder.finish() {
                warn!("Cannot flush recording: {e}");
            }
        }
        stats.video_file = self.video.output();
        stats.video_packets = self.video.packets;
        stats.video_lost = self.video.lost;
        stats.audio_file = self.audio.output();
        stats.audio_packets = self.audio.packets;
    }
}

fn with_suffix(prefix: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = prefix.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::ctp::{Csp, CtpPacket, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, MEDIA_VIDEO, kind};
    use crate::video::media::PT_H264;
    use crate::video::tunnel::{CONTROL_CHANNEL, Control, Frame, UdpmPing};
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::net::TcpStream;

    const TOKEN: [u8; 2] = [0x6b, 0x12];

    fn rtp(seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80, PT_H264];
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0xc0, 0x80, 0x9a, 0x0d]);
        out.extend_from_slice(payload);
        out
    }

    /// Fake external unit replaying the device side of the app capture.
    /// Returns the CSP message types it received from the client.
    async fn fake_device(listener: TcpListener, udp: UdpSocket) -> Vec<u8> {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut buf = vec![];
        let mut chunk = [0u8; 4096];
        let mut ubuf = [0u8; 2048];
        let mut client_udp: Option<SocketAddr> = None;
        let (mut ctpp, mut udpm, mut rtpc) = (0u16, 0u16, None::<u16>);
        let mut dev_tx = 0u8;
        let mut received = vec![];
        let mut client_ctp: Option<CtpPacket> = None;

        loop {
            tokio::select! {
                n = tcp.read(&mut chunk) => {
                    let n = n.unwrap();
                    if n == 0 { return received; }
                    buf.extend_from_slice(&chunk[..n]);
                }
                r = udp.recv_from(&mut ubuf) => {
                    let (n, from) = r.unwrap();
                    client_udp = Some(from);
                    let (frame, _) = Frame::decode(&ubuf[..n]).unwrap();
                    assert_eq!(frame.channel, udpm);
                    let ping = UdpmPing::parse(&frame.body).unwrap();
                    assert_eq!(ping.token, TOKEN);
                    if !ping.is_reply {
                        let reply = UdpmPing { is_reply: true, lane_open: true, ..ping };
                        udp.send_to(&Frame::encode(udpm, &reply.encode()), from).await.unwrap();
                    }
                    continue;
                }
            }
            while let Some((frame, used)) = Frame::decode(&buf) {
                buf.drain(..used);
                if frame.channel == CONTROL_CHANNEL {
                    match Control::parse(&frame.body).unwrap() {
                        Control::Open {
                            name,
                            channel,
                            extra,
                            ..
                        } => {
                            let mut reply = Control::open_response(channel, 0);
                            match &name {
                                b"CTPP" => {
                                    assert_eq!(&extra, b"COMHUB012\0");
                                    ctpp = channel;
                                }
                                b"UDPM" => {
                                    udpm = channel;
                                    reply = Frame::encode(
                                        CONTROL_CHANNEL,
                                        &[&reply[8..], &[2, 0, 0, 0][..], &TOKEN[..]].concat(),
                                    );
                                }
                                b"RTPC" => rtpc = rtpc.or(Some(channel)),
                                _ => panic!("unexpected channel"),
                            }
                            tcp.write_all(&reply).await.unwrap();
                        }
                        Control::Close { .. } => {}
                        other => panic!("unexpected {other:?}"),
                    }
                    continue;
                }
                assert_eq!(frame.channel, ctpp);
                let packet = CtpPacket::parse(&frame.body).unwrap();
                if !packet.is_data() {
                    continue;
                }
                if client_ctp.is_none() {
                    dev_tx = packet.rx;
                }
                client_ctp = Some(packet.clone());
                let reply = |flags: u8, tx: u8, payload: Vec<u8>| {
                    let p = CtpPacket {
                        flags,
                        conn: [packet.conn[0] | 0x80, packet.conn[1]],
                        tx,
                        rx: packet.tx.wrapping_add(1),
                        payload,
                        src: packet.dst,
                        dst: packet.src,
                    };
                    Frame::encode(ctpp, &p.encode())
                };
                tcp.write_all(&reply(0, dev_tx, vec![])).await.unwrap();
                let csp_kind = packet.payload[1];
                received.push(csp_kind);
                match Csp::parse(&packet.payload).unwrap() {
                    Csp::CapabReport { .. } => {
                        for payload in [Csp::capab_report(b'P', 0x1b), vec![0, kind::CONNECT]] {
                            tcp.write_all(&reply(FLAG_DATA, dev_tx, payload))
                                .await
                                .unwrap();
                            dev_tx = dev_tx.wrapping_add(1);
                        }
                    }
                    Csp::MediaReq { action, channel } => {
                        assert_eq!(action, MEDIA_VIDEO);
                        assert_eq!(Some(channel), rtpc);
                        let to = client_udp.expect("UDP lane must be open first");
                        let packets = [
                            rtp(1, &[0x67, 0x42]),
                            rtp(2, &[0x7c, 0x85, 1, 2]),
                            rtp(3, &[0x7c, 0x45, 3]),
                        ];
                        for p in packets {
                            udp.send_to(&Frame::encode(channel, &p), to).await.unwrap();
                        }
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        tcp.write_all(&reply(FLAG_DATA, dev_tx, Csp::release(0)))
                            .await
                            .unwrap();
                        dev_tx = dev_tx.wrapping_add(1);
                    }
                    _ => {}
                }
            }
        }
    }

    #[tokio::test]
    async fn records_video_from_fake_external_unit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let device_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tcp_addr = listener.local_addr().unwrap();
        let udp_addr = device_udp.local_addr().unwrap();
        let device = tokio::spawn(fake_device(listener, device_udp));

        let tcp = TcpStream::connect(tcp_addr).await.unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.connect(udp_addr).await.unwrap();

        let dir = std::env::temp_dir().join(format!("viper-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let options = VideoOptions {
            output: dir.join("door"),
            duration: Duration::from_secs(20),
            audio: false,
            video: VideoParams::default(),
        };
        let target = CallTarget {
            src: LogAddr::new("COMHUB012").unwrap(),
            mode: CallMode::Watch {
                external_unit: LogAddr::new("00000100").unwrap(),
            },
        };
        let stats = run(
            tcp,
            Arc::new(udp),
            0x5174,
            target,
            options,
            std::future::pending(),
        )
        .await
        .unwrap();

        assert!(stats.udp_lane_open);
        assert_eq!(stats.video_packets, 3);
        assert_eq!(stats.video_lost, 0);
        let file = stats.video_file.unwrap();
        assert_eq!(file, dir.join("door.h264"));
        assert_eq!(
            std::fs::read(&file).unwrap(),
            vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 1, 2, 3]
        );

        let received = tokio::time::timeout(Duration::from_secs(2), device)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            received,
            vec![kind::START, kind::CAPAB_REPORT, kind::MEDIAREQ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Fake apartment: accepts the subunit registration, then the external
    /// unit calls us. Returns the CSP message types received on each
    /// connection (registration, call).
    async fn fake_ringing_unit(
        listener: TcpListener,
        udp: UdpSocket,
        unit_closes: bool,
    ) -> (Vec<u8>, Vec<u8>) {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut buf = vec![];
        let mut chunk = [0u8; 4096];
        let mut ubuf = [0u8; 2048];
        let mut client_udp: Option<SocketAddr> = None;
        let (mut ctpp, mut udpm, mut rtpc) = (0u16, 0u16, None::<u16>);
        let (mut on_registration, mut on_call) = (vec![], vec![]);
        let me = LogAddr::new("COMHUB012").unwrap();
        let door = LogAddr::new("00000100").unwrap();
        // the call connection is ours: conn 12 34, we number from 0x50
        let mut call_tx = 0x50u8;
        let mut call_rx: u8;
        let mut keepalive_pending = false;
        let call = |flags: u8, tx: u8, rx: u8, payload: Vec<u8>| CtpPacket {
            flags,
            conn: [0x12, 0x34],
            tx,
            rx,
            payload,
            src: door,
            dst: me,
        };

        loop {
            tokio::select! {
                n = tcp.read(&mut chunk) => {
                    let n = n.unwrap();
                    if n == 0 { return (on_registration, on_call); }
                    buf.extend_from_slice(&chunk[..n]);
                }
                r = udp.recv_from(&mut ubuf) => {
                    let (n, from) = r.unwrap();
                    client_udp = Some(from);
                    let (frame, _) = Frame::decode(&ubuf[..n]).unwrap();
                    let ping = UdpmPing::parse(&frame.body).unwrap();
                    if !ping.is_reply {
                        let reply = UdpmPing { is_reply: true, lane_open: true, ..ping };
                        udp.send_to(&Frame::encode(udpm, &reply.encode()), from).await.unwrap();
                    }
                    continue;
                }
            }
            while let Some((frame, used)) = Frame::decode(&buf) {
                buf.drain(..used);
                if frame.channel == CONTROL_CHANNEL {
                    if let Control::Open { name, channel, .. } =
                        Control::parse(&frame.body).unwrap()
                    {
                        let mut reply = Control::open_response(channel, 0);
                        match &name {
                            b"CTPP" => ctpp = channel,
                            b"UDPM" => {
                                udpm = channel;
                                reply = Frame::encode(
                                    CONTROL_CHANNEL,
                                    &[&reply[8..], &[2, 0, 0, 0][..], &TOKEN[..]].concat(),
                                );
                            }
                            b"RTPC" => rtpc = rtpc.or(Some(channel)),
                            _ => panic!("unexpected channel"),
                        }
                        tcp.write_all(&reply).await.unwrap();
                    }
                    continue;
                }
                assert_eq!(frame.channel, ctpp);
                let packet = CtpPacket::parse(&frame.body).unwrap();

                if packet.conn != [0x92, 0x34] {
                    // registration connection, opened by the client
                    if packet.flags & FLAG_CLOSE != 0 {
                        on_registration.push(FLAG_CLOSE);
                        // registered: the external unit rings the apartment
                        let start = Csp::start(&door, &LogAddr::new("COMHUB01").unwrap(), 9, b'P');
                        let p = call(FLAG_OPEN | FLAG_DATA, call_tx, 0x90, start);
                        tcp.write_all(&Frame::encode(ctpp, &p.encode()))
                            .await
                            .unwrap();
                        continue;
                    }
                    if !packet.is_data() {
                        continue;
                    }
                    assert_eq!(packet.dst, LogAddr::new("COMHUB01").unwrap());
                    let Csp::Other { kind } = Csp::parse(&packet.payload).unwrap() else {
                        panic!("expected REGISTER_REQ");
                    };
                    on_registration.push(kind);
                    let id = [packet.payload[2], packet.payload[3]];
                    let mut res = vec![0x00, kind::REGISTER_RES, id[0], id[1]];
                    res.extend_from_slice(&me.0);
                    res.extend_from_slice(&[0, 0]);
                    for (flags, payload) in [(0u8, vec![]), (FLAG_DATA, res)] {
                        let p = CtpPacket {
                            flags,
                            conn: [packet.conn[0] | 0x80, packet.conn[1]],
                            tx: packet.rx,
                            rx: packet.tx.wrapping_add(1),
                            payload,
                            src: packet.dst,
                            dst: packet.src,
                        };
                        tcp.write_all(&Frame::encode(ctpp, &p.encode()))
                            .await
                            .unwrap();
                    }
                    continue;
                }

                // our call connection, answered by the client with the top bit set
                if packet.flags & FLAG_CLOSE != 0 {
                    on_call.push(FLAG_CLOSE);
                    continue;
                }
                if keepalive_pending && !packet.is_data() {
                    assert_eq!(
                        packet.rx, call_tx,
                        "keepalive must not advance the sequence"
                    );
                    keepalive_pending = false;
                    on_call.push(0);
                    continue;
                }
                if packet.rx == call_tx.wrapping_add(1) {
                    call_tx = call_tx.wrapping_add(1);
                }
                if !packet.is_data() {
                    continue;
                }
                call_rx = packet.tx.wrapping_add(1);
                let ack = call(0, call_tx, call_rx, vec![]);
                tcp.write_all(&Frame::encode(ctpp, &ack.encode()))
                    .await
                    .unwrap();
                on_call.push(packet.payload[1]);
                match Csp::parse(&packet.payload).unwrap() {
                    Csp::Alerting => {
                        // a keepalive must be acked without advancing the sequence
                        let keepalive = call(FLAG_DATA, call_tx, call_rx, vec![]);
                        tcp.write_all(&Frame::encode(ctpp, &keepalive.encode()))
                            .await
                            .unwrap();
                        keepalive_pending = true;
                        let caps = call(FLAG_DATA, call_tx, call_rx, Csp::capab_report(b'P', 0x1b));
                        tcp.write_all(&Frame::encode(ctpp, &caps.encode()))
                            .await
                            .unwrap();
                    }
                    Csp::MediaReq { action, channel } => {
                        assert_eq!(action, MEDIA_VIDEO);
                        assert_eq!(Some(channel), rtpc);
                        let to = client_udp.expect("UDP lane must be open first");
                        for p in [rtp(1, &[0x67, 0x42]), rtp(2, &[0x65, 7])] {
                            udp.send_to(&Frame::encode(channel, &p), to).await.unwrap();
                        }
                        if !unit_closes {
                            continue;
                        }
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        // the unit gives up ringing: it closes the connection
                        let close = call(FLAG_DATA | FLAG_CLOSE, call_tx, call_rx, vec![]);
                        tcp.write_all(&Frame::encode(ctpp, &close.encode()))
                            .await
                            .unwrap();
                    }
                    _ => {}
                }
            }
        }
    }

    async fn listen_to_fake_unit(
        name: &str,
        unit_closes: bool,
        ring_timeout: Duration,
    ) -> (VideoStats, Vec<u8>, Vec<u8>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let device_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tcp_addr = listener.local_addr().unwrap();
        let udp_addr = device_udp.local_addr().unwrap();
        let device = tokio::spawn(fake_ringing_unit(listener, device_udp, unit_closes));

        let tcp = TcpStream::connect(tcp_addr).await.unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.connect(udp_addr).await.unwrap();

        let dir = std::env::temp_dir().join(format!("viper-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let options = VideoOptions {
            output: dir.join("ring"),
            duration: Duration::from_secs(20),
            audio: false,
            video: VideoParams::default(),
        };
        let target = CallTarget {
            src: LogAddr::new("COMHUB012").unwrap(),
            mode: CallMode::Listen {
                apartment: LogAddr::new("COMHUB01").unwrap(),
                ttl_secs: 300,
                ring_timeout,
            },
        };
        let stats = run(
            tcp,
            Arc::new(udp),
            0x5174,
            target,
            options,
            std::future::pending(),
        )
        .await
        .unwrap();
        let video = std::fs::read(stats.video_file.as_ref().unwrap()).unwrap();
        assert_eq!(video, vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 7]);
        std::fs::remove_dir_all(dir).unwrap();

        let (on_registration, on_call) = tokio::time::timeout(Duration::from_secs(2), device)
            .await
            .unwrap()
            .unwrap();
        (stats, on_registration, on_call)
    }

    #[tokio::test]
    async fn records_early_video_of_an_incoming_call() {
        let (stats, on_registration, on_call) =
            listen_to_fake_unit("listen", true, Duration::from_secs(60)).await;

        assert_eq!(stats.caller.as_deref(), Some("\"00000100\""));
        assert_eq!(stats.video_packets, 2);
        assert_eq!(on_registration, vec![kind::REGISTER_REQ, FLAG_CLOSE]);
        // never CONNECT; the unit closed the call, we close our side too
        assert_eq!(
            on_call,
            vec![
                kind::CAPAB_REPORT,
                kind::ALERTING,
                0, // ack of the keepalive
                kind::MEDIAREQ,
                FLAG_CLOSE
            ]
        );
    }

    #[tokio::test]
    async fn releases_our_leg_when_the_ringing_time_elapses() {
        let (_, _, on_call) = listen_to_fake_unit("timeout", false, Duration::from_secs(1)).await;

        // like the app when call_wait_time expires: RELEASE, then close
        assert_eq!(
            on_call,
            vec![
                kind::CAPAB_REPORT,
                kind::ALERTING,
                0, // ack of the keepalive
                kind::MEDIAREQ,
                kind::RELEASE,
                FLAG_CLOSE
            ]
        );
    }
}

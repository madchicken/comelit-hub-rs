//! Video call session: replays the sequence the official app uses to watch an
//! external unit (see `CallFsm` in `libvipcomelit.so`):
//!
//! 1. open the CTPP channel (our address as parameter) and the UDPM channel;
//! 2. on CTPP send CSP START + CAPAB_REPORT to the external unit;
//! 3. ping the UDPM lane over UDP until the device marks it open;
//! 4. once the device sent its CAPAB_REPORT and CONNECT/ALERTING, open one
//!    RTPC channel per stream and send a MEDIAREQ pointing at it;
//! 5. record the RTP packets arriving on the RTPC channels (over UDP, or over
//!    TCP if the UDP lane never opened);
//! 6. on stop, send RELEASE and close all channels.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use super::ctp::{
    APP_CAPABILITIES, CAPAB_VIDEO, Csp, CtpPacket, FLAG_DATA, FLAG_OPEN, LogAddr, MEDIA_AUDIO,
    UNIT_INTERNAL, VideoParams,
};
use super::media::{RtpPacket, StreamRecorder};
use super::tunnel::{CONTROL_CHANNEL, Control, Frame, Transport, UdpmPing};
use crate::ViperError;

const TICK: Duration = Duration::from_millis(100);
const CTP_RETRANSMIT: Duration = Duration::from_secs(1);
const CTP_MAX_ATTEMPTS: u32 = 5;
const UDPM_OPENING_PING: Duration = Duration::from_millis(300);
const UDPM_KEEPALIVE: Duration = Duration::from_secs(5);
/// How long to wait for the UDP lane before letting media flow over TCP.
const UDPM_OPEN_TIMEOUT: Duration = Duration::from_secs(3);
/// How long to wait for CONNECT/ALERTING after the device capabilities.
const PEER_READY_TIMEOUT: Duration = Duration::from_millis(1500);
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const RELEASE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct VideoOptions {
    /// Output path without extension; the extension follows the codec
    /// (`.h264`, `.m4v`, `.alaw`, `.ulaw`).
    pub output: PathBuf,
    /// Stop after this long. The call is also released when `stop` resolves.
    pub duration: Duration,
    /// Also request the external unit audio.
    pub audio: bool,
    pub video: VideoParams,
}

#[derive(Debug, Default)]
pub struct VideoStats {
    pub udp_lane_open: bool,
    pub video_file: Option<PathBuf>,
    pub video_packets: usize,
    pub video_lost: usize,
    pub audio_file: Option<PathBuf>,
    pub audio_packets: usize,
}

/// Addresses of the call: `src` is us (apt address + subaddress), `dst` is
/// the external unit to watch.
pub struct CallTarget {
    pub src: LogAddr,
    pub dst: LogAddr,
}

struct MediaChannel {
    id: u16,
    action: Option<u8>,
    open_sent: bool,
    opened: bool,
    recorder: StreamRecorder,
}

struct InFlight {
    payload: Vec<u8>,
    flags: u8,
    sent_at: Instant,
    attempts: u32,
}

/// Stop-and-wait CTP connection, as observed in the app capture.
struct Ctp {
    conn: [u8; 2],
    tx: u8,
    rx: u8,
    peer_seen: bool,
    opened: bool,
    src: LogAddr,
    dst: LogAddr,
    queue: VecDeque<Vec<u8>>,
    in_flight: Option<InFlight>,
}

impl Ctp {
    fn packet(&self, flags: u8, payload: Vec<u8>) -> CtpPacket {
        CtpPacket {
            flags,
            conn: self.conn,
            tx: self.tx,
            rx: self.rx,
            payload,
            src: self.src,
            dst: self.dst,
        }
    }

    fn is_ours(&self, packet: &CtpPacket) -> bool {
        packet.conn[0] & 0x7f == self.conn[0] && packet.conn[1] == self.conn[1]
    }
}

struct Session<'a> {
    tcp: TcpStream,
    rbuf: Vec<u8>,
    udp: &'a UdpSocket,
    ctpp: u16,
    ctpp_open: bool,
    udpm: u16,
    udpm_token: Option<[u8; 2]>,
    udpm_seq: u8,
    udpm_opened_at: Option<Instant>,
    lane_open: bool,
    next_ping: Instant,
    ctp: Ctp,
    peer_caps: Option<(u32, Instant)>,
    peer_ready: bool,
    released: bool,
    video: MediaChannel,
    audio: Option<MediaChannel>,
    params: VideoParams,
}

pub async fn run(
    tcp: TcpStream,
    udp: &UdpSocket,
    first_channel: u16,
    target: CallTarget,
    options: VideoOptions,
    stop: impl Future<Output = ()>,
) -> Result<VideoStats, ViperError> {
    // ThreadRng is !Send: draw everything up front so the future stays Send.
    let (conn, tx, rx, udpm_seq) = {
        let mut rng = rand::rng();
        let conn: [u8; 2] = [rng.random_range(0x01..0x80), rng.random()];
        (conn, rng.random(), rng.random(), rng.random())
    };
    let started = Instant::now();
    let media = |id: u16, action: Option<u8>, suffix: &str| MediaChannel {
        id,
        action,
        open_sent: false,
        opened: false,
        recorder: StreamRecorder::new(with_suffix(&options.output, suffix)),
    };
    let mut session = Session {
        tcp,
        rbuf: Vec::with_capacity(64 * 1024),
        udp,
        ctpp: first_channel,
        ctpp_open: false,
        udpm: first_channel + 1,
        udpm_token: None,
        udpm_seq,
        udpm_opened_at: None,
        lane_open: false,
        next_ping: started,
        ctp: Ctp {
            conn,
            tx,
            rx,
            peer_seen: false,
            opened: false,
            src: target.src,
            dst: target.dst,
            queue: VecDeque::new(),
            in_flight: None,
        },
        peer_caps: None,
        peer_ready: false,
        released: false,
        video: media(first_channel + 2, None, ""),
        audio: options
            .audio
            .then(|| media(first_channel + 3, Some(MEDIA_AUDIO), "-audio")),
        params: options.video,
    };

    let result = session.drive(started, options.duration, stop).await;
    session.teardown().await;
    result?;

    let mut stats = VideoStats {
        udp_lane_open: session.lane_open,
        video_file: session.video.recorder.output(),
        video_packets: session.video.recorder.packets,
        video_lost: session.video.recorder.lost,
        ..Default::default()
    };
    if let Some(audio) = &session.audio {
        stats.audio_file = audio.recorder.output();
        stats.audio_packets = audio.recorder.packets;
    }
    Ok(stats)
}

fn with_suffix(prefix: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = prefix.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

impl Session<'_> {
    async fn drive(
        &mut self,
        started: Instant,
        duration: Duration,
        stop: impl Future<Output = ()>,
    ) -> Result<(), ViperError> {
        tokio::pin!(stop);
        let mut tick = interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut ubuf = vec![0u8; 65536];

        let our_address = self.ctp.src.0;
        self.send(&Control::open(
            "CTPP",
            self.ctpp,
            Transport::Tcp,
            &our_address,
        ))
        .await?;
        self.send(&Control::open("UDPM", self.udpm, Transport::Udp, &[]))
            .await?;
        info!(
            "Calling {:?} from {:?} (CTPP 0x{:04x}, UDPM 0x{:04x})",
            self.ctp.dst, self.ctp.src, self.ctpp, self.udpm
        );

        let deadline = started + duration;
        loop {
            tokio::select! {
                read = self.tcp.read_buf(&mut self.rbuf) => {
                    if read? == 0 {
                        return Err(ViperError::Generic("Connection closed by the device".into()));
                    }
                    while let Some((frame, used)) = Frame::decode(&self.rbuf) {
                        self.rbuf.drain(..used);
                        self.on_tcp_frame(frame).await?;
                    }
                }
                received = self.udp.recv(&mut ubuf) => {
                    match received {
                        Ok(n) => self.on_udp(&ubuf[..n]).await?,
                        // ICMP port unreachable and the like: keep going over TCP
                        Err(e) => debug!("UDP receive error: {e}"),
                    }
                }
                _ = tick.tick() => self.on_tick(started).await?,
                _ = &mut stop => {
                    info!("Stop requested");
                    return Ok(());
                }
            }
            if self.released {
                info!("Call released by the device");
                return Ok(());
            }
            if Instant::now() >= deadline {
                info!("Recording time elapsed");
                return Ok(());
            }
        }
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<(), ViperError> {
        self.tcp.write_all(bytes).await?;
        Ok(())
    }

    async fn on_tcp_frame(&mut self, frame: Frame) -> Result<(), ViperError> {
        match frame.channel {
            CONTROL_CHANNEL => self.on_control(&frame.body).await,
            ch if ch == self.ctpp => self.on_ctp(&frame.body).await,
            ch => {
                // RTPC traffic stays on TCP while the UDP lane is closed
                self.on_media(ch, &frame.body);
                Ok(())
            }
        }
    }

    async fn on_control(&mut self, body: &[u8]) -> Result<(), ViperError> {
        match Control::parse(body) {
            Some(Control::OpenResponse {
                channel,
                status,
                extra,
            }) => {
                if status != 0 {
                    return Err(ViperError::Generic(format!(
                        "Device refused to open channel 0x{channel:04x} (status {status})"
                    )));
                }
                if channel == self.ctpp {
                    debug!("CTPP channel open");
                    self.ctpp_open = true;
                    let call_id: u32 = rand::rng().random();
                    let (src, dst) = (self.ctp.src, self.ctp.dst);
                    self.ctp
                        .queue
                        .push_back(Csp::start(&src, &dst, call_id, UNIT_INTERNAL));
                    self.ctp
                        .queue
                        .push_back(Csp::capab_report(UNIT_INTERNAL, APP_CAPABILITIES));
                    self.pump_ctp().await?;
                } else if channel == self.udpm {
                    if extra.len() < 2 {
                        warn!("UDPM open response without token, media will use TCP");
                    } else {
                        self.udpm_token = Some([extra[0], extra[1]]);
                        self.udpm_opened_at = Some(Instant::now());
                        debug!("UDPM channel open, token {:02x?}", &extra[..2]);
                    }
                } else if let Some(media) = self.media_mut(channel) {
                    media.opened = true;
                    let id = media.id;
                    let request = match media.action {
                        None => Csp::video_request(id, &self.params),
                        Some(action) => Csp::media_request(action, id),
                    };
                    info!("RTPC channel 0x{id:04x} open, sending MEDIAREQ");
                    self.ctp.queue.push_back(request);
                    self.pump_ctp().await?;
                }
            }
            Some(Control::Open { name, channel, .. }) => {
                // The device opens its own RTPC to receive our audio: accept it
                // like the app does, even though we never send anything.
                debug!(
                    "Device opened {} channel 0x{channel:04x}",
                    String::from_utf8_lossy(&name)
                );
                self.send(&Control::open_response(channel, 0)).await?;
            }
            Some(Control::Close { channel }) => {
                debug!("Device closed channel 0x{channel:04x}");
                if channel == self.ctpp {
                    self.released = true;
                }
            }
            None => debug!("Unknown control message {body:02x?}"),
        }
        Ok(())
    }

    async fn on_ctp(&mut self, body: &[u8]) -> Result<(), ViperError> {
        let Some(packet) = CtpPacket::parse(body) else {
            debug!("Malformed CTP packet {body:02x?}");
            return Ok(());
        };
        if !self.ctp.is_ours(&packet) {
            debug!("Ignoring CTP packet for connection {:02x?}", packet.conn);
            return Ok(());
        }

        if self.ctp.in_flight.is_some() && packet.rx == self.ctp.tx.wrapping_add(1) {
            self.ctp.in_flight = None;
            self.ctp.tx = self.ctp.tx.wrapping_add(1);
        }

        if packet.is_data() {
            if !self.ctp.peer_seen {
                self.ctp.peer_seen = true;
                self.ctp.rx = packet.tx;
            }
            let fresh = packet.tx == self.ctp.rx;
            if fresh {
                self.ctp.rx = self.ctp.rx.wrapping_add(1);
            }
            let ack = self.ctp.packet(0x00, vec![]).encode();
            self.send(&Frame::encode(self.ctpp, &ack)).await?;
            if fresh {
                self.on_csp(&packet.payload);
            } else {
                debug!("Duplicate CTP packet {:02x}", packet.tx);
            }
        }

        self.pump_ctp().await
    }

    fn on_csp(&mut self, payload: &[u8]) {
        match Csp::parse(payload) {
            Some(Csp::CapabReport {
                unit_type,
                capabilities,
            }) => {
                info!(
                    "Device '{}' capabilities 0x{capabilities:x}",
                    unit_type as char
                );
                if capabilities & CAPAB_VIDEO == 0 {
                    warn!("The device does not announce video capability");
                }
                self.peer_caps = Some((capabilities, Instant::now()));
            }
            Some(Csp::Alerting) => {
                info!("Device alerting");
                self.peer_ready = true;
            }
            Some(Csp::Connect) => {
                info!("Device connected");
                self.peer_ready = true;
            }
            Some(Csp::Release { cause }) => {
                info!("Device released the call (cause {cause})");
                self.released = true;
            }
            Some(Csp::MediaReq { action, channel }) => debug!(
                "Device asks for our media 0x{action:02x} on channel 0x{channel:04x}, ignored"
            ),
            other => debug!("CSP message {other:?}"),
        }
    }

    /// Sends the next queued CSP message if nothing is waiting for an ack,
    /// and retransmits the pending one when its ack is late.
    async fn pump_ctp(&mut self) -> Result<(), ViperError> {
        let now = Instant::now();
        let resend = match &mut self.ctp.in_flight {
            Some(pending) if now.duration_since(pending.sent_at) >= CTP_RETRANSMIT => {
                if pending.attempts >= CTP_MAX_ATTEMPTS {
                    return Err(ViperError::Generic(
                        "The device does not acknowledge CTP messages".into(),
                    ));
                }
                pending.attempts += 1;
                pending.sent_at = now;
                Some((pending.flags, pending.payload.clone()))
            }
            Some(_) => None,
            None => self.ctp.queue.pop_front().map(|payload| {
                let flags = if self.ctp.opened {
                    FLAG_DATA
                } else {
                    FLAG_OPEN | FLAG_DATA
                };
                self.ctp.opened = true;
                self.ctp.in_flight = Some(InFlight {
                    payload: payload.clone(),
                    flags,
                    sent_at: now,
                    attempts: 1,
                });
                (flags, payload)
            }),
        };
        if let Some((flags, payload)) = resend {
            let packet = self.ctp.packet(flags, payload).encode();
            self.send(&Frame::encode(self.ctpp, &packet)).await?;
        }
        Ok(())
    }

    async fn on_udp(&mut self, datagram: &[u8]) -> Result<(), ViperError> {
        let mut rest = datagram;
        while let Some((frame, used)) = Frame::decode(rest) {
            rest = &rest[used..];
            if frame.channel != self.udpm {
                self.on_media(frame.channel, &frame.body);
                continue;
            }
            let Some(ping) = UdpmPing::parse(&frame.body) else {
                continue;
            };
            if Some(ping.token) != self.udpm_token {
                debug!("UDPM ping with unknown token {:02x?}", ping.token);
                continue;
            }
            if !ping.is_reply {
                let reply = UdpmPing {
                    is_reply: true,
                    lane_open: true,
                    ..ping
                };
                self.udp
                    .send(&Frame::encode(self.udpm, &reply.encode()))
                    .await?;
            }
            if ping.lane_open && !self.lane_open {
                info!("UDP lane open");
                self.lane_open = true;
            }
        }
        Ok(())
    }

    fn on_media(&mut self, channel: u16, body: &[u8]) {
        let Some(media) = self.media_mut(channel) else {
            debug!("Data on unknown channel 0x{channel:04x}");
            return;
        };
        let Some(packet) = RtpPacket::parse(body) else {
            debug!("Non RTP data on channel 0x{channel:04x}");
            return;
        };
        if media.recorder.packets == 0 {
            info!(
                "First RTP packet on 0x{channel:04x}, payload type {}",
                packet.payload_type
            );
        }
        if let Err(e) = media.recorder.push(&packet) {
            warn!("Cannot record RTP packet: {e}");
        }
    }

    async fn on_tick(&mut self, started: Instant) -> Result<(), ViperError> {
        let now = Instant::now();
        if !self.ctp.peer_seen && now.duration_since(started) > SETUP_TIMEOUT {
            return Err(ViperError::Generic(
                "No answer from the external unit".into(),
            ));
        }

        if let Some(token) = self.udpm_token
            && now >= self.next_ping
        {
            let ping = UdpmPing {
                token,
                is_reply: false,
                seq: self.udpm_seq,
                lane_open: self.lane_open,
            };
            self.udpm_seq = self.udpm_seq.wrapping_add(1);
            if let Err(e) = self
                .udp
                .send(&Frame::encode(self.udpm, &ping.encode()))
                .await
            {
                debug!("UDPM ping failed: {e}");
            }
            self.next_ping = now
                + if self.lane_open {
                    UDPM_KEEPALIVE
                } else {
                    UDPM_OPENING_PING
                };
        }

        self.maybe_request_media(now).await?;
        self.pump_ctp().await
    }

    async fn maybe_request_media(&mut self, now: Instant) -> Result<(), ViperError> {
        if self.video.open_sent {
            return Ok(());
        }
        let Some((_, caps_at)) = self.peer_caps else {
            return Ok(());
        };
        if !self.peer_ready && now.duration_since(caps_at) < PEER_READY_TIMEOUT {
            return Ok(());
        }
        let lane_timed_out = self
            .udpm_opened_at
            .is_none_or(|at| now.duration_since(at) >= UDPM_OPEN_TIMEOUT);
        if !self.lane_open && !lane_timed_out {
            return Ok(());
        }
        if !self.lane_open {
            warn!("UDP lane not open, media will be requested over TCP");
        }

        let mut opens = vec![Control::open("RTPC", self.video.id, Transport::Udp, &[])];
        self.video.open_sent = true;
        if let Some(audio) = self.audio.as_mut() {
            audio.open_sent = true;
            opens.push(Control::open("RTPC", audio.id, Transport::Udp, &[]));
        }
        for open in opens {
            self.send(&open).await?;
        }
        Ok(())
    }

    fn media_mut(&mut self, channel: u16) -> Option<&mut MediaChannel> {
        if channel == self.video.id {
            return Some(&mut self.video);
        }
        self.audio.as_mut().filter(|audio| audio.id == channel)
    }

    /// Best effort: release the call, close every channel, flush recordings.
    async fn teardown(&mut self) {
        if self.ctp.opened && !self.released {
            self.ctp.queue.clear();
            self.ctp.queue.push_back(Csp::release(0));
            let until = Instant::now() + RELEASE_TIMEOUT;
            let _ = tokio::time::timeout_at(until, async {
                loop {
                    self.pump_ctp().await?;
                    if self.ctp.in_flight.is_none() && self.ctp.queue.is_empty() {
                        return Ok::<_, ViperError>(());
                    }
                    let mut chunk = [0u8; 4096];
                    let n = self.tcp.read(&mut chunk).await?;
                    if n == 0 {
                        return Ok(());
                    }
                    self.rbuf.extend_from_slice(&chunk[..n]);
                    while let Some((frame, used)) = Frame::decode(&self.rbuf) {
                        self.rbuf.drain(..used);
                        if frame.channel == self.ctpp {
                            self.on_ctp(&frame.body).await?;
                        }
                    }
                }
            })
            .await;
        }

        let mut channels = vec![];
        if self.video.open_sent {
            channels.push(self.video.id);
        }
        if let Some(audio) = self.audio.as_ref().filter(|a| a.open_sent) {
            channels.push(audio.id);
        }
        channels.extend([self.udpm, self.ctpp]);
        for channel in channels {
            if let Err(e) = self.send(&Control::close(channel)).await {
                debug!("Cannot close channel 0x{channel:04x}: {e}");
                break;
            }
        }

        for recorder in std::iter::once(&mut self.video.recorder)
            .chain(self.audio.as_mut().map(|a| &mut a.recorder))
        {
            if let Err(e) = recorder.finish() {
                warn!("Cannot flush recording: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::ctp::{MEDIA_VIDEO, kind};
    use crate::video::media::PT_H264;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

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
            dst: LogAddr::new("00000100").unwrap(),
        };
        let stats = run(tcp, &udp, 0x5174, target, options, std::future::pending())
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
}

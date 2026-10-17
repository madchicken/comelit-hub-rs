//! Video call session, replaying what the official app does (see `CallFsm`
//! and `Subunit` in `libvipcomelit.so`). Two ways to get the stream:
//!
//! * [`CallMode::Watch`] ("auto-ignition"): we call the external unit with a
//!   CSP START. The unit refuses it with USER_BUSY while it is ringing.
//! * [`CallMode::Listen`]: we register as a subunit of the apartment
//!   (REGISTER_REQ to the apartment main address) and wait for the unit to
//!   call us when someone rings. We answer CAPAB_REPORT + ALERTING, never
//!   CONNECT, so the call keeps ringing everywhere and we only get the early
//!   video, like the app showing who is at the door before answering.
//!
//! In both cases:
//! 1. open the CTPP channel (our address as parameter) and the UDPM channel;
//! 2. ping the UDPM lane over UDP until the device marks it open;
//! 3. once the unit sent its CAPAB_REPORT (and CONNECT/ALERTING when
//!    watching), open one RTPC channel per stream and send a MEDIAREQ on the
//!    call connection pointing at it;
//! 4. record the RTP packets arriving on the RTPC channels (over UDP, or over
//!    TCP if the UDP lane never opened);
//! 5. on stop, release our leg of the call with RELEASE and close its CTP
//!    connection, then close all channels. An incoming call is kept for at
//!    most `ring_timeout`, like the app's own ringing timer
//!    (`call_wait_time`, `CallFsm::st_in_alerting` on 0xb0e): other units
//!    ring with their own timers, and releasing our leg does not stop them.

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
    APP_CAPABILITIES, CAPAB_VIDEO, Csp, CtpPacket, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, LogAddr,
    MEDIA_AUDIO, UNIT_INTERNAL, VideoParams,
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

/// Stop-and-wait CTP connection, as observed in the app capture. The side
/// accepting a connection sets the top bit of the connection id.
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
    /// A connection we open towards `dst`; `tx`/`rx` are the initial
    /// sequences we propose for both sides.
    fn initiate(src: LogAddr, dst: LogAddr) -> Ctp {
        let mut rng = rand::rng();
        Ctp {
            conn: [rng.random_range(0x01..0x80), rng.random()],
            tx: rng.random(),
            rx: rng.random(),
            peer_seen: false,
            opened: false,
            src,
            dst,
            queue: VecDeque::new(),
            in_flight: None,
        }
    }

    /// A connection opened by the device with `first`; it proposes our
    /// initial sequence in its rx field.
    fn accept(first: &CtpPacket) -> Ctp {
        Ctp {
            conn: [first.conn[0] ^ 0x80, first.conn[1]],
            tx: first.rx,
            rx: first.tx,
            peer_seen: true,
            opened: true,
            src: first.dst,
            dst: first.src,
            queue: VecDeque::new(),
            in_flight: None,
        }
    }

    fn matches(&self, packet: &CtpPacket) -> bool {
        packet.conn == [self.conn[0] ^ 0x80, self.conn[1]]
    }

    fn packet(&self, flags: u8, payload: Vec<u8>) -> Vec<u8> {
        CtpPacket {
            flags,
            conn: self.conn,
            tx: self.tx,
            rx: self.rx,
            payload,
            src: self.src,
            dst: self.dst,
        }
        .encode()
    }

    /// Handles a packet of this connection: returns the ack to send, if any,
    /// and the payload when it is new data.
    fn receive(&mut self, packet: &CtpPacket) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        if self.in_flight.is_some() && packet.rx == self.tx.wrapping_add(1) {
            self.in_flight = None;
            self.tx = self.tx.wrapping_add(1);
        }
        if !packet.is_data() {
            return (None, None);
        }
        if !self.peer_seen {
            self.peer_seen = true;
            self.rx = packet.tx;
        }
        // An empty data packet is a keepalive: it must be acknowledged but
        // does not consume a sequence number (FUN_00084b54 only advances the
        // expected sequence for packets with a payload).
        if packet.payload.is_empty() {
            return (Some(self.packet(0x00, vec![])), None);
        }
        let fresh = packet.tx == self.rx;
        if fresh {
            self.rx = self.rx.wrapping_add(1);
        } else {
            debug!("Duplicate CTP packet {:02x}", packet.tx);
        }
        let ack = self.packet(0x00, vec![]);
        (Some(ack), fresh.then(|| packet.payload.clone()))
    }

    /// The next packet to send: a retransmission of the unacknowledged one
    /// when its ack is late, otherwise the next queued message.
    fn poll(&mut self, now: Instant) -> Result<Option<Vec<u8>>, ViperError> {
        let send = match &mut self.in_flight {
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
            None => self.queue.pop_front().map(|payload| {
                let flags = if self.opened {
                    FLAG_DATA
                } else {
                    FLAG_OPEN | FLAG_DATA
                };
                self.opened = true;
                self.in_flight = Some(InFlight {
                    payload: payload.clone(),
                    flags,
                    sent_at: now,
                    attempts: 1,
                });
                (flags, payload)
            }),
        };
        Ok(send.map(|(flags, payload)| self.packet(flags, payload)))
    }

    fn idle(&self) -> bool {
        self.in_flight.is_none() && self.queue.is_empty()
    }
}

struct Registration {
    ctp: Ctp,
    id: u16,
    apartment: LogAddr,
    ttl_secs: u16,
}

struct Session<'a> {
    tcp: TcpStream,
    rbuf: Vec<u8>,
    udp: &'a UdpSocket,
    src: LogAddr,
    ctpp: u16,
    ctpp_open: bool,
    udpm: u16,
    udpm_token: Option<[u8; 2]>,
    udpm_seq: u8,
    udpm_opened_at: Option<Instant>,
    lane_open: bool,
    next_ping: Instant,
    /// The connection carrying the call: ours when watching, the device's
    /// when listening.
    call: Option<Ctp>,
    /// Pending call towards the external unit, started once CTPP is open.
    watch: Option<LogAddr>,
    registration: Option<Registration>,
    listening: bool,
    caller: Option<LogAddr>,
    ring_timeout: Option<Duration>,
    /// When we stop watching the incoming call.
    ring_deadline: Option<Instant>,
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
    let udpm_seq = rand::rng().random();
    let started = Instant::now();
    let media = |id: u16, action: Option<u8>, suffix: &str| MediaChannel {
        id,
        action,
        open_sent: false,
        opened: false,
        recorder: StreamRecorder::new(with_suffix(&options.output, suffix)),
    };
    let (watch, registration, ring_timeout) = match target.mode {
        CallMode::Watch { external_unit } => (Some(external_unit), None, None),
        CallMode::Listen {
            apartment,
            ttl_secs,
            ring_timeout,
        } => (
            None,
            Some(Registration {
                ctp: Ctp::initiate(target.src, apartment),
                id: rand::rng().random(),
                apartment,
                ttl_secs,
            }),
            Some(ring_timeout),
        ),
    };
    let mut session = Session {
        tcp,
        rbuf: Vec::with_capacity(64 * 1024),
        udp,
        src: target.src,
        ctpp: first_channel,
        ctpp_open: false,
        udpm: first_channel + 1,
        udpm_token: None,
        udpm_seq,
        udpm_opened_at: None,
        lane_open: false,
        next_ping: started,
        call: None,
        listening: registration.is_some(),
        watch,
        registration,
        caller: None,
        ring_timeout,
        ring_deadline: None,
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
        caller: session.caller.map(|c| format!("{c:?}")),
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

        let our_address = self.src.0;
        self.send(&Control::open(
            "CTPP",
            self.ctpp,
            Transport::Tcp,
            &our_address,
        ))
        .await?;
        self.send(&Control::open("UDPM", self.udpm, Transport::Udp, &[]))
            .await?;
        match (&self.watch, &self.registration) {
            (Some(unit), _) => info!(
                "Calling {unit:?} from {:?} (CTPP 0x{:04x}, UDPM 0x{:04x})",
                self.src, self.ctpp, self.udpm
            ),
            (_, Some(reg)) => info!(
                "Registering {:?} with {:?}, then waiting for a call",
                self.src, reg.apartment
            ),
            _ => {}
        }

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
            if self.ring_deadline.is_some_and(|d| Instant::now() >= d) {
                info!("Ringing time elapsed");
                return Ok(());
            }
        }
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<(), ViperError> {
        self.tcp.write_all(bytes).await?;
        Ok(())
    }

    async fn send_ctp(&mut self, packet: Vec<u8>) -> Result<(), ViperError> {
        let frame = Frame::encode(self.ctpp, &packet);
        self.send(&frame).await
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
                    if let Some(unit) = self.watch.take() {
                        let mut call = Ctp::initiate(self.src, unit);
                        let call_id: u32 = rand::rng().random();
                        call.queue
                            .push_back(Csp::start(&self.src, &unit, call_id, UNIT_INTERNAL));
                        call.queue
                            .push_back(Csp::capab_report(UNIT_INTERNAL, APP_CAPABILITIES));
                        self.call = Some(call);
                    }
                    if let Some(reg) = self.registration.as_mut() {
                        let request = Csp::register_request(reg.id, &reg.ctp.src, reg.ttl_secs);
                        reg.ctp.queue.push_back(request);
                    }
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
                    if let Some(call) = self.call.as_mut() {
                        call.queue.push_back(request);
                    }
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

        if let Some(reg) = self.registration.as_mut()
            && reg.ctp.matches(&packet)
        {
            let (ack, payload) = reg.ctp.receive(&packet);
            if let Some(ack) = ack {
                self.send_ctp(ack).await?;
            }
            if let Some(payload) = payload {
                self.on_registration(&payload).await?;
            }
            return self.pump_ctp().await;
        }

        let incoming = self.call.is_none()
            && self.listening
            && packet.is_data()
            && packet.flags & FLAG_OPEN != 0
            && matches!(Csp::parse(&packet.payload), Some(Csp::Start { .. }));
        if incoming {
            self.call = Some(Ctp::accept(&packet));
        }

        let Some(call) = self.call.as_mut().filter(|c| c.matches(&packet)) else {
            debug!("Ignoring CTP packet for connection {:02x?}", packet.conn);
            return Ok(());
        };
        let (ack, payload) = call.receive(&packet);
        let close = (packet.flags & FLAG_CLOSE != 0).then(|| call.packet(FLAG_CLOSE, vec![]));
        if let Some(ack) = ack {
            self.send_ctp(ack).await?;
        }
        if let Some(close) = close {
            // ctp_read reports CTP_DISCONNECTED once the peer closed, and the
            // app closes its side too: the call is over
            info!("Device closed the call connection");
            self.send_ctp(close).await?;
            self.released = true;
            return Ok(());
        }
        if let Some(payload) = payload {
            if Csp::parse(&payload).is_none() {
                debug!("Unknown CSP payload {payload:02x?} in {body:02x?}");
            }
            self.on_csp(&payload);
        }
        self.pump_ctp().await
    }

    async fn on_registration(&mut self, payload: &[u8]) -> Result<(), ViperError> {
        let Some(reg) = self.registration.as_mut() else {
            return Ok(());
        };
        match Csp::parse(payload) {
            Some(Csp::RegisterRes {
                id,
                address,
                status,
            }) if id == reg.id && address == reg.ctp.src => {
                if status != 0 {
                    return Err(ViperError::Generic(format!(
                        "Registration refused by {:?} (status {status})",
                        reg.apartment
                    )));
                }
                info!(
                    "Registered with {:?} for {} s, waiting for a call",
                    reg.apartment, reg.ttl_secs
                );
                // like Subunit::su_st_registering: close the registration
                // connection once the response arrived
                let close = reg.ctp.packet(FLAG_CLOSE, vec![]);
                self.registration = None;
                self.send_ctp(close).await?;
            }
            other => debug!("Unexpected message during registration: {other:?}"),
        }
        Ok(())
    }

    fn on_csp(&mut self, payload: &[u8]) {
        match Csp::parse(payload) {
            Some(Csp::Start {
                src,
                dst,
                unit_type,
            }) => {
                info!(
                    "Incoming call from {src:?} ('{}') to {dst:?}",
                    unit_type as char
                );
                self.caller = Some(src);
                self.peer_ready = true;
                self.ring_deadline = self.ring_timeout.map(|t| Instant::now() + t);
                if let Some(call) = self.call.as_mut() {
                    // CallFsm::initNewConnectionStart: announce ourselves and
                    // ring, without ever answering
                    call.queue
                        .push_back(Csp::capab_report(UNIT_INTERNAL, APP_CAPABILITIES));
                    call.queue.push_back(Csp::alerting());
                }
            }
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

    /// Sends what the CTP connections have to send (new messages or
    /// retransmissions).
    async fn pump_ctp(&mut self) -> Result<(), ViperError> {
        let now = Instant::now();
        let mut packets = vec![];
        if let Some(reg) = self.registration.as_mut() {
            packets.extend(reg.ctp.poll(now)?);
        }
        if let Some(call) = self.call.as_mut() {
            packets.extend(call.poll(now)?);
        }
        for packet in packets {
            self.send_ctp(packet).await?;
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
        let waiting_for_peer = match (&self.call, &self.registration) {
            (_, Some(reg)) => !reg.ctp.peer_seen,
            (Some(call), None) => !self.listening && !call.peer_seen,
            (None, None) => !self.listening,
        };
        if waiting_for_peer && now.duration_since(started) > SETUP_TIMEOUT {
            return Err(ViperError::Generic(if self.listening {
                "The apartment did not answer the registration".into()
            } else {
                "No answer from the external unit".into()
            }));
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
        if self.video.open_sent || self.call.is_none() {
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

    /// Best effort: release our leg of the call and close its connection
    /// (like the app when its ringing timer expires), close every channel,
    /// flush recordings.
    async fn teardown(&mut self) {
        let release = !self.released && self.call.as_ref().is_some_and(|c| c.opened);
        if release {
            if let Some(call) = self.call.as_mut() {
                call.queue.clear();
                call.queue.push_back(Csp::release(0));
            }
            let until = Instant::now() + RELEASE_TIMEOUT;
            let _ = tokio::time::timeout_at(until, async {
                loop {
                    self.pump_ctp().await?;
                    if self.call.as_ref().is_none_or(Ctp::idle) {
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
            if let Some(close) = self.call.as_ref().map(|c| c.packet(FLAG_CLOSE, vec![])) {
                let _ = self.send_ctp(close).await;
            }
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
            mode: CallMode::Watch {
                external_unit: LogAddr::new("00000100").unwrap(),
            },
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
        let stats = run(tcp, &udp, 0x5174, target, options, std::future::pending())
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

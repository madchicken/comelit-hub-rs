//! A long lived ViP unit, the way the official app keeps itself available
//! (`VipUnitImpl`, `Subunit` and `CallFsm` in `libvipcomelit.so`):
//!
//! * it keeps the CTPP and UDPM channels open, pinging the UDP lane;
//! * when configured with an apartment address it registers as a subunit
//!   (REGISTER_REQ to the apartment main address) and renews the
//!   registration before it expires, so calls to the apartment reach it;
//! * an incoming call (someone ringing) is answered with CAPAB_REPORT +
//!   ALERTING, never CONNECT, so every other unit keeps ringing; our leg is
//!   released after `ring_timeout`, like the app's `call_wait_time`;
//! * media is started on demand: on the ringing call if there is one,
//!   otherwise by calling the configured entrance (auto-ignition). RTP
//!   packets are handed to the caller as they arrive.
//!
//! The unit runs in its own task; [`ViperUnit`] sends it commands and
//! [`UnitEvent`]s report what happens. The task ends with an error when the
//! connection with the device is lost.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use super::ctp::{
    APP_CAPABILITIES, CAPAB_VIDEO, Csp, CtpPacket, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, LogAddr,
    MEDIA_AUDIO, UNIT_INTERNAL, VideoParams,
};
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
/// How long a peer has to answer a call or registration we start.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the ack of our RELEASE before closing anyway.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(1);
/// Retry delay after a registration got no answer.
const REGISTRATION_RETRY: Duration = Duration::from_secs(30);
const EVENT_QUEUE: usize = 32;
const MEDIA_QUEUE: usize = 1024;

#[derive(Debug, Clone)]
pub struct UnitConfig {
    /// Our logical address (apartment address + subaddress).
    pub src: LogAddr,
    /// Register as a subunit of this apartment address to receive calls.
    pub apartment: Option<LogAddr>,
    /// Lifetime requested for the registration, renewed before it expires.
    pub registration_ttl_secs: u16,
    /// External unit called (auto-ignition) when media is requested while
    /// nobody is ringing.
    pub entrance: Option<LogAddr>,
    /// How long we keep our leg of an incoming call.
    pub ring_timeout: Duration,
    pub video: VideoParams,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitEvent {
    /// The apartment accepted (or renewed) our registration.
    Registered,
    /// The UDP lane is open: media flows over UDP.
    UdpLaneOpen,
    /// Someone is ringing: `caller` is the external unit.
    Ringing { caller: LogAddr },
    /// The incoming call ended (answered elsewhere, abandoned, or our ring
    /// timeout elapsed).
    RingEnded,
    /// The media requested with [`ViperUnit::start_media`] stopped.
    MediaEnded { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

/// An RTP packet as sent by the external unit.
#[derive(Debug, Clone)]
pub struct MediaPacket {
    pub kind: MediaKind,
    pub rtp: Vec<u8>,
}

enum Command {
    StartMedia {
        audio: bool,
        sink: mpsc::Sender<MediaPacket>,
        reply: oneshot::Sender<Result<(), ViperError>>,
    },
    StopMedia,
    RequestKeyFrame,
    Shutdown,
}

/// A running unit: its handle, its events, and the task (ending with the
/// error that stopped it).
pub type SpawnedUnit = (
    ViperUnit,
    mpsc::Receiver<UnitEvent>,
    JoinHandle<Result<(), ViperError>>,
);

/// Handle on a running unit.
#[derive(Clone)]
pub struct ViperUnit {
    commands: mpsc::Sender<Command>,
}

impl ViperUnit {
    /// Starts the unit on an authorized tunnel connection. `first_channel`
    /// is the first channel id it may use.
    pub fn spawn(
        tcp: TcpStream,
        udp: Arc<UdpSocket>,
        first_channel: u16,
        config: UnitConfig,
    ) -> SpawnedUnit {
        let (commands, command_rx) = mpsc::channel(8);
        let (event_tx, events) = mpsc::channel(EVENT_QUEUE);
        let unit = Unit::new(tcp, udp, first_channel, config, event_tx);
        let task = tokio::spawn(unit.run(command_rx));
        (ViperUnit { commands }, events, task)
    }

    /// Requests the external unit video (and audio): on the ringing call if
    /// there is one, otherwise by calling the configured entrance. Replaces
    /// a previous request. Packets arrive on the returned channel until
    /// [`UnitEvent::MediaEnded`] or [`ViperUnit::stop_media`].
    pub async fn start_media(
        &self,
        audio: bool,
    ) -> Result<mpsc::Receiver<MediaPacket>, ViperError> {
        let (sink, packets) = mpsc::channel(MEDIA_QUEUE);
        let (reply, result) = oneshot::channel();
        self.send(Command::StartMedia { audio, sink, reply })
            .await?;
        result
            .await
            .map_err(|_| ViperError::Generic("The video unit stopped".into()))??;
        Ok(packets)
    }

    /// Stops the media; a call we started for it is released, a ringing
    /// call keeps ringing.
    pub async fn stop_media(&self) -> Result<(), ViperError> {
        self.send(Command::StopMedia).await
    }

    /// Asks the external unit for a key frame (SPS, PPS and IDR): it only
    /// sends them when the video starts, so a consumer joining a running
    /// stream needs one. Like `CallFsm::requestVideoKeyFrameGeneration`,
    /// this repeats the video MEDIAREQ.
    pub async fn request_key_frame(&self) -> Result<(), ViperError> {
        self.send(Command::RequestKeyFrame).await
    }

    /// Releases any call, closes the channels and ends the unit task.
    pub async fn shutdown(&self) -> Result<(), ViperError> {
        self.send(Command::Shutdown).await
    }

    async fn send(&self, command: Command) -> Result<(), ViperError> {
        self.commands
            .send(command)
            .await
            .map_err(|_| ViperError::Generic("The video unit stopped".into()))
    }
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
    created: Instant,
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
            created: Instant::now(),
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
            created: Instant::now(),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallKind {
    /// Someone is ringing; we keep our leg until `deadline`.
    Incoming { deadline: Instant },
    /// We called the entrance (auto-ignition).
    Watch,
}

struct Call {
    ctp: Ctp,
    kind: CallKind,
    peer_caps: Option<Instant>,
    peer_ready: bool,
    /// RELEASE queued: close the connection once it is acknowledged.
    releasing: Option<Instant>,
}

struct Rtpc {
    id: u16,
    kind: MediaKind,
    opened: bool,
}

struct Media {
    sink: mpsc::Sender<MediaPacket>,
    audio: bool,
    channels: Vec<Rtpc>,
}

struct Unit {
    tcp: TcpStream,
    rbuf: Vec<u8>,
    udp: Arc<UdpSocket>,
    config: UnitConfig,
    events: mpsc::Sender<UnitEvent>,
    next_channel: u16,
    ctpp: u16,
    ctpp_open: bool,
    udpm: u16,
    udpm_token: Option<[u8; 2]>,
    udpm_seq: u8,
    udpm_opened_at: Option<Instant>,
    lane_open: bool,
    next_ping: Instant,
    registration: Option<Registration>,
    next_registration: Option<Instant>,
    call: Option<Call>,
    media: Option<Media>,
    /// Packets to send on CTPP for connections we already dropped.
    farewells: Vec<Vec<u8>>,
}

impl Unit {
    fn new(
        tcp: TcpStream,
        udp: Arc<UdpSocket>,
        first_channel: u16,
        config: UnitConfig,
        events: mpsc::Sender<UnitEvent>,
    ) -> Unit {
        Unit {
            tcp,
            rbuf: Vec::with_capacity(64 * 1024),
            udp,
            config,
            events,
            next_channel: first_channel.wrapping_add(2),
            ctpp: first_channel,
            ctpp_open: false,
            udpm: first_channel.wrapping_add(1),
            udpm_token: None,
            udpm_seq: rand::rng().random(),
            udpm_opened_at: None,
            lane_open: false,
            next_ping: Instant::now(),
            registration: None,
            next_registration: None,
            call: None,
            media: None,
            farewells: vec![],
        }
    }

    async fn run(mut self, mut commands: mpsc::Receiver<Command>) -> Result<(), ViperError> {
        let result = self.drive(&mut commands).await;
        self.teardown().await;
        result
    }

    async fn drive(&mut self, commands: &mut mpsc::Receiver<Command>) -> Result<(), ViperError> {
        let mut tick = interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut ubuf = vec![0u8; 65536];
        let udp = self.udp.clone();

        let our_address = self.config.src.0;
        self.send(&Control::open(
            "CTPP",
            self.ctpp,
            Transport::Tcp,
            &our_address,
        ))
        .await?;
        self.send(&Control::open("UDPM", self.udpm, Transport::Udp, &[]))
            .await?;

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
                received = udp.recv(&mut ubuf) => {
                    match received {
                        Ok(n) => self.on_udp(&ubuf[..n]).await?,
                        // ICMP port unreachable and the like: keep going over TCP
                        Err(e) => debug!("UDP receive error: {e}"),
                    }
                }
                _ = tick.tick() => self.on_tick().await?,
                command = commands.recv() => match command {
                    Some(Command::StartMedia { audio, sink, reply }) => {
                        let result = self.start_media(audio, sink).await;
                        let _ = reply.send(result);
                    }
                    Some(Command::StopMedia) => self.stop_media(None).await?,
                    Some(Command::RequestKeyFrame) => self.request_key_frame().await?,
                    Some(Command::Shutdown) | None => return Ok(()),
                },
            }
        }
    }

    fn emit(&self, event: UnitEvent) {
        if self.events.try_send(event).is_err() {
            debug!("Unit event dropped: nobody is listening");
        }
    }

    fn allocate_channel(&mut self) -> u16 {
        let id = self.next_channel;
        self.next_channel = match self.next_channel.wrapping_add(1) & 0x7fff {
            0 => 1,
            next => next,
        };
        id
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<(), ViperError> {
        self.tcp.write_all(bytes).await?;
        Ok(())
    }

    async fn send_ctp(&mut self, packet: Vec<u8>) -> Result<(), ViperError> {
        let frame = Frame::encode(self.ctpp, &packet);
        self.send(&frame).await
    }

    // --- media requests ------------------------------------------------------

    async fn start_media(
        &mut self,
        audio: bool,
        sink: mpsc::Sender<MediaPacket>,
    ) -> Result<(), ViperError> {
        if self.media.is_some() {
            self.close_media_channels().await?;
        }
        if self.call.is_none() {
            if self.ctpp_open {
                self.start_watch()?;
            } else if self.config.entrance.is_none() {
                // nothing to call: media can only come from a ringing call
                return Err(ViperError::Generic(
                    "Nobody is ringing and no entrance is configured".into(),
                ));
            }
            // otherwise the call starts as soon as CTPP is open
        }
        self.media = Some(Media {
            sink,
            audio,
            channels: vec![],
        });
        Ok(())
    }

    /// Calls the entrance, for media while nobody is ringing.
    fn start_watch(&mut self) -> Result<(), ViperError> {
        let Some(entrance) = self.config.entrance else {
            return Err(ViperError::Generic(
                "Nobody is ringing and no entrance is configured".into(),
            ));
        };
        info!("Calling {entrance:?} for its video");
        let mut ctp = Ctp::initiate(self.config.src, entrance);
        let call_id: u32 = rand::rng().random();
        ctp.queue.push_back(Csp::start(
            &self.config.src,
            &entrance,
            call_id,
            UNIT_INTERNAL,
        ));
        ctp.queue
            .push_back(Csp::capab_report(UNIT_INTERNAL, APP_CAPABILITIES));
        self.call = Some(Call {
            ctp,
            kind: CallKind::Watch,
            peer_caps: None,
            peer_ready: false,
            releasing: None,
        });
        Ok(())
    }

    /// Stops the media; `reason` set means it stopped by itself and is
    /// reported with [`UnitEvent::MediaEnded`].
    async fn stop_media(&mut self, reason: Option<String>) -> Result<(), ViperError> {
        if self.media.is_none() {
            return Ok(());
        }
        self.close_media_channels().await?;
        self.media = None;
        if let Some(reason) = reason {
            info!("Media ended: {reason}");
            self.emit(UnitEvent::MediaEnded { reason });
        }
        if self
            .call
            .as_ref()
            .is_some_and(|c| c.kind == CallKind::Watch)
        {
            self.release_call();
        }
        Ok(())
    }

    async fn close_media_channels(&mut self) -> Result<(), ViperError> {
        let channels: Vec<u16> = self
            .media
            .as_mut()
            .map(|m| m.channels.drain(..).map(|c| c.id).collect())
            .unwrap_or_default();
        for id in channels {
            self.send(&Control::close(id)).await?;
        }
        Ok(())
    }

    async fn request_key_frame(&mut self) -> Result<(), ViperError> {
        let video = self.media.as_ref().and_then(|m| {
            m.channels
                .iter()
                .find(|c| c.kind == MediaKind::Video && c.opened)
                .map(|c| c.id)
        });
        let (Some(id), Some(call)) = (video, self.call.as_mut()) else {
            // no video yet: the first frames will include one anyway
            return Ok(());
        };
        debug!("Requesting a key frame");
        call.ctp
            .queue
            .push_back(Csp::video_request(id, &self.config.video));
        self.pump_ctp().await
    }

    /// Opens the RTPC channels once the call can carry media; the MEDIAREQ
    /// follows when the device confirms each channel.
    async fn maybe_request_media(&mut self, now: Instant) -> Result<(), ViperError> {
        let Some(media) = self.media.as_ref() else {
            return Ok(());
        };
        if !media.channels.is_empty() {
            return Ok(());
        }
        let Some(call) = self.call.as_ref().filter(|c| c.releasing.is_none()) else {
            return Ok(());
        };
        let Some(caps_at) = call.peer_caps else {
            return Ok(());
        };
        if !call.peer_ready && now.duration_since(caps_at) < PEER_READY_TIMEOUT {
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

        let mut kinds = vec![MediaKind::Video];
        if media.audio {
            kinds.push(MediaKind::Audio);
        }
        for kind in kinds {
            let id = self.allocate_channel();
            self.send(&Control::open("RTPC", id, Transport::Udp, &[]))
                .await?;
            if let Some(media) = self.media.as_mut() {
                media.channels.push(Rtpc {
                    id,
                    kind,
                    opened: false,
                });
            }
        }
        Ok(())
    }

    fn on_media(&mut self, channel: u16, body: &[u8]) {
        let Some(media) = self.media.as_ref() else {
            debug!("Data on channel 0x{channel:04x} without media request");
            return;
        };
        let Some(rtpc) = media.channels.iter().find(|c| c.id == channel) else {
            debug!("Data on unknown channel 0x{channel:04x}");
            return;
        };
        let packet = MediaPacket {
            kind: rtpc.kind,
            rtp: body.to_vec(),
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = media.sink.try_send(packet) {
            debug!("Media consumer too slow, packet dropped");
        }
    }

    // --- calls ---------------------------------------------------------------

    /// Queues RELEASE on the current call; the connection is closed once
    /// it is acknowledged (or after [`RELEASE_TIMEOUT`]).
    fn release_call(&mut self) {
        if let Some(call) = self.call.as_mut().filter(|c| c.releasing.is_none()) {
            call.ctp.queue.clear();
            call.ctp.queue.push_back(Csp::release(0));
            call.releasing = Some(Instant::now());
        }
    }

    /// Forgets the current call, after the peer released or closed it.
    async fn call_ended(&mut self, reason: &str) -> Result<(), ViperError> {
        let Some(call) = self.call.take() else {
            return Ok(());
        };
        match call.kind {
            CallKind::Incoming { .. } => {
                info!("Incoming call ended: {reason}");
                self.emit(UnitEvent::RingEnded);
                if self.media.is_some() {
                    // keep showing the entrance once it stops ringing
                    self.close_media_channels().await?;
                    if self.start_watch().is_err() {
                        self.stop_media(Some(format!("the call ended: {reason}")))
                            .await?;
                    }
                }
            }
            CallKind::Watch => {
                info!("Call to the entrance ended: {reason}");
                self.stop_media(Some(format!("the entrance ended the call: {reason}")))
                    .await?;
            }
        }
        Ok(())
    }

    fn on_csp(&mut self, payload: &[u8]) -> Option<String> {
        let call = self.call.as_mut()?;
        match Csp::parse(payload) {
            Some(Csp::CapabReport {
                unit_type,
                capabilities,
            }) => {
                debug!(
                    "Device '{}' capabilities 0x{capabilities:x}",
                    unit_type as char
                );
                if capabilities & CAPAB_VIDEO == 0 {
                    warn!("The device does not announce video capability");
                }
                call.peer_caps = Some(Instant::now());
            }
            Some(Csp::Alerting) | Some(Csp::Connect) => call.peer_ready = true,
            Some(Csp::Release { cause }) => return Some(format!("released (cause {cause})")),
            Some(Csp::MediaReq { action, channel }) => debug!(
                "Device asks for our media 0x{action:02x} on channel 0x{channel:04x}, ignored"
            ),
            other => debug!("CSP message {other:?}"),
        }
        None
    }

    async fn accept_incoming(&mut self, packet: &CtpPacket) -> Result<(), ViperError> {
        let Some(Csp::Start {
            src,
            dst,
            unit_type,
        }) = Csp::parse(&packet.payload)
        else {
            return Ok(());
        };
        info!(
            "Incoming call from {src:?} ('{}') to {dst:?}",
            unit_type as char
        );
        if let Some(previous) = self.call.take() {
            // someone rings while we watch the entrance: the incoming call
            // takes over, the media follows it
            let close = previous.ctp.packet(FLAG_CLOSE, vec![]);
            self.farewells.push(close);
            self.close_media_channels().await?;
        }
        let mut ctp = Ctp::accept(packet);
        // CallFsm::initNewConnectionStart: announce ourselves and ring,
        // without ever answering
        ctp.queue
            .push_back(Csp::capab_report(UNIT_INTERNAL, APP_CAPABILITIES));
        ctp.queue.push_back(Csp::alerting());
        self.call = Some(Call {
            ctp,
            kind: CallKind::Incoming {
                deadline: Instant::now() + self.config.ring_timeout,
            },
            peer_caps: None,
            peer_ready: true,
            releasing: None,
        });
        self.emit(UnitEvent::Ringing { caller: src });
        Ok(())
    }

    // --- registration --------------------------------------------------------

    fn start_registration(&mut self) {
        let Some(apartment) = self.config.apartment else {
            return;
        };
        let mut ctp = Ctp::initiate(self.config.src, apartment);
        let id: u16 = rand::rng().random();
        ctp.queue.push_back(Csp::register_request(
            id,
            &self.config.src,
            self.config.registration_ttl_secs,
        ));
        self.registration = Some(Registration { ctp, id });
        self.next_registration = None;
    }

    async fn on_registration(&mut self, payload: &[u8]) -> Result<(), ViperError> {
        let Some(reg) = self.registration.as_ref() else {
            return Ok(());
        };
        match Csp::parse(payload) {
            Some(Csp::RegisterRes {
                id,
                address,
                status,
            }) if id == reg.id && address == reg.ctp.src => {
                // like Subunit::su_st_registering: close the registration
                // connection once the response arrived
                let close = reg.ctp.packet(FLAG_CLOSE, vec![]);
                self.registration = None;
                self.send_ctp(close).await?;
                if status != 0 {
                    warn!("Registration refused (status {status}), retrying later");
                    self.next_registration = Some(Instant::now() + REGISTRATION_RETRY);
                    return Ok(());
                }
                let ttl = Duration::from_secs(self.config.registration_ttl_secs.into());
                info!("Registered for {} s", ttl.as_secs());
                self.next_registration = Some(Instant::now() + ttl * 9 / 10);
                self.emit(UnitEvent::Registered);
            }
            other => debug!("Unexpected message during registration: {other:?}"),
        }
        Ok(())
    }

    // --- tunnel traffic ------------------------------------------------------

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
                if channel == self.ctpp || channel == self.udpm {
                    if status != 0 {
                        return Err(ViperError::Generic(format!(
                            "Device refused to open channel 0x{channel:04x} (status {status})"
                        )));
                    }
                } else if status != 0 {
                    self.stop_media(Some(format!("RTPC refused (status {status})")))
                        .await?;
                    return Ok(());
                }
                if channel == self.ctpp {
                    debug!("CTPP channel open");
                    self.ctpp_open = true;
                    self.start_registration();
                    if self.media.is_some() && self.call.is_none() && self.start_watch().is_err() {
                        self.stop_media(Some("no call to take media from".into()))
                            .await?;
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
                } else {
                    let request = self.media.as_mut().and_then(|media| {
                        let rtpc = media.channels.iter_mut().find(|c| c.id == channel)?;
                        rtpc.opened = true;
                        Some(match rtpc.kind {
                            MediaKind::Video => Csp::video_request(rtpc.id, &self.config.video),
                            MediaKind::Audio => Csp::media_request(MEDIA_AUDIO, rtpc.id),
                        })
                    });
                    if let Some(request) = request {
                        debug!("RTPC channel 0x{channel:04x} open, sending MEDIAREQ");
                        if let Some(call) = self.call.as_mut() {
                            call.ctp.queue.push_back(request);
                        }
                        self.pump_ctp().await?;
                    }
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
                    return Err(ViperError::Generic("The device closed CTPP".into()));
                }
            }
            Some(Control::CloseResponse { channel, status }) => {
                debug!("Channel 0x{channel:04x} closed (status {status})");
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

        let incoming = self.config.apartment.is_some()
            && packet.is_data()
            && packet.flags & FLAG_OPEN != 0
            && !self.call.as_ref().is_some_and(|c| c.ctp.matches(&packet))
            && matches!(Csp::parse(&packet.payload), Some(Csp::Start { .. }));
        if incoming {
            if self
                .call
                .as_ref()
                .is_some_and(|c| matches!(c.kind, CallKind::Incoming { .. }))
            {
                debug!("Second incoming call while ringing, ignored");
                return Ok(());
            }
            self.accept_incoming(&packet).await?;
        }

        let Some(call) = self.call.as_mut().filter(|c| c.ctp.matches(&packet)) else {
            debug!("Ignoring CTP packet for connection {:02x?}", packet.conn);
            return Ok(());
        };
        let (ack, payload) = call.ctp.receive(&packet);
        let close = (packet.flags & FLAG_CLOSE != 0).then(|| call.ctp.packet(FLAG_CLOSE, vec![]));
        if let Some(ack) = ack {
            self.send_ctp(ack).await?;
        }
        if let Some(close) = close {
            // ctp_read reports CTP_DISCONNECTED once the peer closed, and the
            // app closes its side too: the call is over
            self.send_ctp(close).await?;
            return self.call_ended("the device closed the connection").await;
        }
        if let Some(payload) = payload
            && let Some(reason) = self.on_csp(&payload)
        {
            let close = self.call.as_ref().map(|c| c.ctp.packet(FLAG_CLOSE, vec![]));
            self.pump_ctp().await?;
            if let Some(close) = close {
                self.send_ctp(close).await?;
            }
            return self.call_ended(&reason).await;
        }
        self.pump_ctp().await
    }

    /// Sends what the CTP connections have to send (new messages or
    /// retransmissions).
    async fn pump_ctp(&mut self) -> Result<(), ViperError> {
        let now = Instant::now();
        let mut packets = std::mem::take(&mut self.farewells);
        if let Some(reg) = self.registration.as_mut() {
            packets.extend(reg.ctp.poll(now)?);
        }
        if let Some(call) = self.call.as_mut() {
            match call.ctp.poll(now) {
                Ok(packet) => packets.extend(packet),
                Err(e) => {
                    warn!("Call lost: {e}");
                    self.call = None;
                    self.emit_call_lost().await?;
                }
            }
        }
        for packet in packets {
            self.send_ctp(packet).await?;
        }
        Ok(())
    }

    async fn emit_call_lost(&mut self) -> Result<(), ViperError> {
        self.stop_media(Some("the call was lost".into())).await
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
                self.emit(UnitEvent::UdpLaneOpen);
            }
        }
        Ok(())
    }

    async fn on_tick(&mut self) -> Result<(), ViperError> {
        let now = Instant::now();

        if self
            .registration
            .as_ref()
            .is_some_and(|r| !r.ctp.peer_seen && now.duration_since(r.ctp.created) > ANSWER_TIMEOUT)
        {
            warn!("The apartment did not answer the registration, retrying later");
            self.registration = None;
            self.next_registration = Some(now + REGISTRATION_RETRY);
        }
        if self.registration.is_none() && self.next_registration.is_some_and(|at| now >= at) {
            self.start_registration();
        }

        if let Some(call) = self.call.as_ref() {
            let unanswered = call.kind == CallKind::Watch
                && !call.ctp.peer_seen
                && now.duration_since(call.ctp.created) > ANSWER_TIMEOUT;
            let ring_over = matches!(call.kind, CallKind::Incoming { deadline } if now >= deadline)
                && call.releasing.is_none();
            let released = call
                .releasing
                .is_some_and(|at| call.ctp.idle() || now.duration_since(at) >= RELEASE_TIMEOUT);
            if unanswered {
                self.call = None;
                self.stop_media(Some("the entrance did not answer".into()))
                    .await?;
            } else if ring_over {
                info!("Ringing time elapsed, releasing our leg of the call");
                self.release_call();
            } else if released {
                let call = self.call.take().expect("checked above");
                self.send_ctp(call.ctp.packet(FLAG_CLOSE, vec![])).await?;
                if matches!(call.kind, CallKind::Incoming { .. }) {
                    self.emit(UnitEvent::RingEnded);
                    self.stop_media(Some("the ringing time elapsed".into()))
                        .await?;
                }
            }
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

        if self.media.as_ref().is_some_and(|m| m.sink.is_closed()) {
            self.stop_media(None).await?;
        }
        self.maybe_request_media(now).await?;
        self.pump_ctp().await
    }

    /// Best effort: release our leg of the call and close its connection
    /// (like the app when its ringing timer expires), close every channel.
    async fn teardown(&mut self) {
        let release = self
            .call
            .as_ref()
            .is_some_and(|c| c.ctp.opened && c.releasing.is_none());
        if release {
            self.release_call();
        }
        if self.call.is_some() {
            let until = Instant::now() + RELEASE_TIMEOUT;
            let _ = tokio::time::timeout_at(until, async {
                loop {
                    self.pump_ctp().await?;
                    if self.call.as_ref().is_none_or(|c| c.ctp.idle()) {
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
                        if frame.channel == self.ctpp
                            && let Some(packet) = CtpPacket::parse(&frame.body)
                            && let Some(call) =
                                self.call.as_mut().filter(|c| c.ctp.matches(&packet))
                        {
                            let (ack, _) = call.ctp.receive(&packet);
                            if let Some(ack) = ack {
                                self.send_ctp(ack).await?;
                            }
                        }
                    }
                }
            })
            .await;
            if let Some(close) = self.call.take().map(|c| c.ctp.packet(FLAG_CLOSE, vec![])) {
                let _ = self.send_ctp(close).await;
            }
        }

        let mut channels: Vec<u16> = self
            .media
            .take()
            .map(|m| m.channels.into_iter().map(|c| c.id).collect())
            .unwrap_or_default();
        channels.extend([self.udpm, self.ctpp]);
        for channel in channels {
            if let Err(e) = self.send(&Control::close(channel)).await {
                debug!("Cannot close channel 0x{channel:04x}: {e}");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;

    use tokio::net::TcpListener;

    use super::*;
    use crate::video::ctp::kind;
    use crate::video::media::PT_H264;

    const TOKEN: [u8; 2] = [0x6b, 0x12];
    const CALL_CONN: [u8; 2] = [0x12, 0x34];

    fn me() -> LogAddr {
        LogAddr::new("COMHUB012").unwrap()
    }
    fn apartment() -> LogAddr {
        LogAddr::new("COMHUB01").unwrap()
    }
    fn door() -> LogAddr {
        LogAddr::new("00000100").unwrap()
    }

    /// What the fake hub saw, per connection as the client addresses it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Seen {
        Register,
        Start([u8; 2]),
        Capab([u8; 2]),
        Alerting([u8; 2]),
        MediaReq([u8; 2]),
        Release([u8; 2]),
        Close([u8; 2]),
    }

    /// A scripted apartment + external unit. `ring` makes the unit call the
    /// client; everything the client sends is reported on `seen`.
    async fn fake_hub(
        listener: TcpListener,
        udp: UdpSocket,
        mut ring: mpsc::Receiver<()>,
        seen: mpsc::UnboundedSender<Seen>,
    ) {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let (mut buf, mut chunk, mut ubuf) = (vec![], [0u8; 4096], [0u8; 2048]);
        let mut client_udp: Option<SocketAddr> = None;
        let (mut ctpp, mut udpm) = (0u16, 0u16);
        // device tx per connection, keyed by the id the client uses
        let mut dev_tx: HashMap<[u8; 2], u8> = HashMap::new();
        let mut registrations: Vec<[u8; 2]> = vec![];

        loop {
            tokio::select! {
                n = tcp.read(&mut chunk) => {
                    let n = n.unwrap();
                    if n == 0 { return; }
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
                Some(()) = ring.recv() => {
                    let start = Csp::start(&door(), &apartment(), 9, b'P');
                    let p = CtpPacket {
                        flags: FLAG_OPEN | FLAG_DATA,
                        conn: CALL_CONN,
                        tx: 0x50,
                        rx: 0x90,
                        payload: start,
                        src: door(),
                        dst: me(),
                    };
                    dev_tx.insert([CALL_CONN[0] ^ 0x80, CALL_CONN[1]], 0x51);
                    tcp.write_all(&Frame::encode(ctpp, &p.encode())).await.unwrap();
                    continue;
                }
            }
            while let Some((frame, used)) = Frame::decode(&buf) {
                buf.drain(..used);
                if frame.channel == CONTROL_CHANNEL {
                    if let Some(Control::Open { name, channel, .. }) = Control::parse(&frame.body) {
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
                            _ => {}
                        }
                        tcp.write_all(&reply).await.unwrap();
                    }
                    continue;
                }
                assert_eq!(frame.channel, ctpp);
                let packet = CtpPacket::parse(&frame.body).unwrap();
                let conn = packet.conn;
                if packet.flags & FLAG_CLOSE != 0 {
                    // closing a registration connection is routine
                    if !registrations.contains(&conn) {
                        seen.send(Seen::Close(conn)).unwrap();
                    }
                    continue;
                }
                if !packet.is_data() || packet.payload.is_empty() {
                    continue;
                }
                let tx = *dev_tx.entry(conn).or_insert(packet.rx);
                let reply = |flags: u8, payload: Vec<u8>, tx: u8| CtpPacket {
                    flags,
                    conn: [conn[0] ^ 0x80, conn[1]],
                    tx,
                    rx: packet.tx.wrapping_add(1),
                    payload,
                    src: packet.dst,
                    dst: packet.src,
                };
                let ack = reply(0, vec![], tx);
                tcp.write_all(&Frame::encode(ctpp, &ack.encode()))
                    .await
                    .unwrap();
                let mut answers = vec![];
                match Csp::parse(&packet.payload).unwrap() {
                    Csp::Other {
                        kind: kind::REGISTER_REQ,
                    } => {
                        registrations.push(conn);
                        seen.send(Seen::Register).unwrap();
                        let mut res = vec![0x00, kind::REGISTER_RES];
                        res.extend_from_slice(&packet.payload[2..4]);
                        res.extend_from_slice(&me().0);
                        res.extend_from_slice(&[0, 0]);
                        answers.push(res);
                    }
                    Csp::Start { .. } => {
                        seen.send(Seen::Start(conn)).unwrap();
                        answers.push(Csp::capab_report(b'P', 0x1b));
                        answers.push(vec![0, kind::CONNECT]);
                    }
                    Csp::CapabReport { .. } => seen.send(Seen::Capab(conn)).unwrap(),
                    Csp::Alerting => {
                        seen.send(Seen::Alerting(conn)).unwrap();
                        answers.push(Csp::capab_report(b'P', 0x1b));
                    }
                    Csp::MediaReq { channel, .. } => {
                        seen.send(Seen::MediaReq(conn)).unwrap();
                        let to = client_udp.expect("UDP lane must be open first");
                        for seq in 1..=2u16 {
                            let mut rtp = vec![0x80, PT_H264];
                            rtp.extend_from_slice(&seq.to_be_bytes());
                            rtp.extend_from_slice(&[0; 8]);
                            rtp.extend_from_slice(&[0x65, seq as u8]);
                            udp.send_to(&Frame::encode(channel, &rtp), to)
                                .await
                                .unwrap();
                        }
                    }
                    Csp::Release { .. } => seen.send(Seen::Release(conn)).unwrap(),
                    _ => {}
                }
                let mut tx = tx;
                for payload in answers {
                    let p = reply(FLAG_DATA, payload, tx);
                    tcp.write_all(&Frame::encode(ctpp, &p.encode()))
                        .await
                        .unwrap();
                    tx = tx.wrapping_add(1);
                }
                dev_tx.insert(conn, tx);
            }
        }
    }

    struct Harness {
        unit: ViperUnit,
        events: mpsc::Receiver<UnitEvent>,
        task: JoinHandle<Result<(), ViperError>>,
        ring: mpsc::Sender<()>,
        seen: mpsc::UnboundedReceiver<Seen>,
    }

    async fn start(config: UnitConfig) -> Harness {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tcp_addr = listener.local_addr().unwrap();
        let udp_addr = hub_udp.local_addr().unwrap();
        let (ring, ring_rx) = mpsc::channel(1);
        let (seen_tx, seen) = mpsc::unbounded_channel();
        tokio::spawn(fake_hub(listener, hub_udp, ring_rx, seen_tx));

        let tcp = TcpStream::connect(tcp_addr).await.unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.connect(udp_addr).await.unwrap();
        let (unit, events, task) = ViperUnit::spawn(tcp, Arc::new(udp), 0x5174, config);
        Harness {
            unit,
            events,
            task,
            ring,
            seen,
        }
    }

    fn config(apartment_: bool, entrance: bool, ring_timeout: Duration) -> UnitConfig {
        UnitConfig {
            src: me(),
            apartment: apartment_.then(apartment),
            registration_ttl_secs: 300,
            entrance: entrance.then(door),
            ring_timeout,
            video: VideoParams::default(),
        }
    }

    async fn next_event(h: &mut Harness) -> UnitEvent {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), h.events.recv())
                .await
                .expect("no unit event")
                .expect("unit stopped");
            if event != UnitEvent::UdpLaneOpen {
                return event;
            }
        }
    }

    async fn next_seen(seen: &mut mpsc::UnboundedReceiver<Seen>) -> Seen {
        tokio::time::timeout(Duration::from_secs(5), seen.recv())
            .await
            .expect("hub saw nothing")
            .unwrap()
    }

    async fn next_packet(packets: &mut mpsc::Receiver<MediaPacket>) -> MediaPacket {
        tokio::time::timeout(Duration::from_secs(5), packets.recv())
            .await
            .expect("no media")
            .expect("media ended")
    }

    #[tokio::test]
    async fn renews_the_registration_before_it_expires() {
        let mut h = start(UnitConfig {
            registration_ttl_secs: 1,
            ..config(true, false, Duration::from_secs(60))
        })
        .await;
        for _ in 0..3 {
            assert_eq!(next_seen(&mut h.seen).await, Seen::Register);
            assert_eq!(next_event(&mut h).await, UnitEvent::Registered);
        }
        h.unit.shutdown().await.unwrap();
        h.task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn signals_a_ring_and_releases_our_leg_after_the_timeout() {
        let mut h = start(config(true, false, Duration::from_millis(500))).await;
        assert_eq!(next_seen(&mut h.seen).await, Seen::Register);
        assert_eq!(next_event(&mut h).await, UnitEvent::Registered);

        h.ring.send(()).await.unwrap();
        assert_eq!(
            next_event(&mut h).await,
            UnitEvent::Ringing { caller: door() }
        );
        let ours = [CALL_CONN[0] ^ 0x80, CALL_CONN[1]];
        assert_eq!(next_seen(&mut h.seen).await, Seen::Capab(ours));
        assert_eq!(next_seen(&mut h.seen).await, Seen::Alerting(ours));
        // nobody asked for the video: no MEDIAREQ, just RELEASE at the end
        assert_eq!(next_seen(&mut h.seen).await, Seen::Release(ours));
        assert_eq!(next_seen(&mut h.seen).await, Seen::Close(ours));
        assert_eq!(next_event(&mut h).await, UnitEvent::RingEnded);

        h.unit.shutdown().await.unwrap();
        h.task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_ring_takes_over_the_entrance_video() {
        let mut h = start(config(true, true, Duration::from_secs(60))).await;
        assert_eq!(next_seen(&mut h.seen).await, Seen::Register);
        assert_eq!(next_event(&mut h).await, UnitEvent::Registered);

        // nobody rings: the unit calls the entrance
        let mut packets = h.unit.start_media(false).await.unwrap();
        let Seen::Start(watch) = next_seen(&mut h.seen).await else {
            panic!("expected a call to the entrance");
        };
        assert_eq!(next_seen(&mut h.seen).await, Seen::Capab(watch));
        assert_eq!(next_seen(&mut h.seen).await, Seen::MediaReq(watch));
        assert_eq!(next_packet(&mut packets).await.kind, MediaKind::Video);

        // someone rings: the call to the entrance is dropped, the media
        // moves to the ringing call
        h.ring.send(()).await.unwrap();
        assert_eq!(next_seen(&mut h.seen).await, Seen::Close(watch));
        assert_eq!(
            next_event(&mut h).await,
            UnitEvent::Ringing { caller: door() }
        );
        let ours = [CALL_CONN[0] ^ 0x80, CALL_CONN[1]];
        assert_eq!(next_seen(&mut h.seen).await, Seen::Capab(ours));
        assert_eq!(next_seen(&mut h.seen).await, Seen::Alerting(ours));
        assert_eq!(next_seen(&mut h.seen).await, Seen::MediaReq(ours));
        while packets.try_recv().is_ok() {}
        assert_eq!(next_packet(&mut packets).await.kind, MediaKind::Video);

        // stopping the media leaves the ringing call alone
        h.unit.stop_media().await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(h.seen.try_recv().is_err());

        // shutting down releases our leg
        h.unit.shutdown().await.unwrap();
        h.task.await.unwrap().unwrap();
        assert_eq!(next_seen(&mut h.seen).await, Seen::Release(ours));
        assert_eq!(next_seen(&mut h.seen).await, Seen::Close(ours));
    }

    #[tokio::test]
    async fn stopping_the_media_releases_the_entrance_call() {
        let mut h = start(config(false, true, Duration::from_secs(60))).await;
        let mut packets = h.unit.start_media(false).await.unwrap();
        let Seen::Start(watch) = next_seen(&mut h.seen).await else {
            panic!("expected a call to the entrance");
        };
        assert_eq!(next_seen(&mut h.seen).await, Seen::Capab(watch));
        assert_eq!(next_seen(&mut h.seen).await, Seen::MediaReq(watch));
        next_packet(&mut packets).await;

        h.unit.stop_media().await.unwrap();
        assert_eq!(next_seen(&mut h.seen).await, Seen::Release(watch));
        assert_eq!(next_seen(&mut h.seen).await, Seen::Close(watch));
        // a stop we asked for is not reported as an event
        while let Ok(event) = h.events.try_recv() {
            assert_eq!(event, UnitEvent::UdpLaneOpen);
        }

        h.unit.shutdown().await.unwrap();
        h.task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_key_frame_request_repeats_the_video_mediareq() {
        let mut h = start(config(false, true, Duration::from_secs(60))).await;
        let mut packets = h.unit.start_media(false).await.unwrap();
        let Seen::Start(watch) = next_seen(&mut h.seen).await else {
            panic!("expected a call to the entrance");
        };
        assert_eq!(next_seen(&mut h.seen).await, Seen::Capab(watch));
        assert_eq!(next_seen(&mut h.seen).await, Seen::MediaReq(watch));
        next_packet(&mut packets).await;

        h.unit.request_key_frame().await.unwrap();
        assert_eq!(next_seen(&mut h.seen).await, Seen::MediaReq(watch));

        h.unit.shutdown().await.unwrap();
        h.task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn media_without_ring_nor_entrance_is_refused() {
        let h = start(config(true, false, Duration::from_secs(60))).await;
        assert!(h.unit.start_media(false).await.is_err());
        h.unit.shutdown().await.unwrap();
        h.task.await.unwrap().unwrap();
    }
}

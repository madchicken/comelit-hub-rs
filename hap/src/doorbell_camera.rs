//! HomeKit camera of the video doorbell, fed by the ViP door entry system.
//!
//! The external unit already sends H.264 Baseline in RTP packets smaller than
//! the HomeKit MTU, so no transcoding is needed: each packet is forwarded to
//! the iOS device with the payload type, SSRC and sequence numbers HomeKit
//! negotiated, encrypted with SRTP (AES_CM_128_HMAC_SHA1_80). An RTCP sender
//! report is sent every second on the same port (rtcp-mux), as HomeKit
//! expects. The audio is encoded to Opus (`crate::doorbell_audio`).
//!
//! Snapshots are decoded from the same video: the first complete frame with
//! an IDR is decoded with OpenH264 and encoded as JPEG. Capturing one while
//! nobody rings turns the external unit camera on: the video then keeps
//! running for a while, so that the snapshots the Home app asks every 10
//! seconds come from the running stream.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hap::accessory::camera::manager::{SessionState, StreamOptions};
use hap::accessory::camera::media::MediaProvider;
use hap::accessory::camera::protocol::StreamInfo;
use image::ExtendedColorType;
use image::codecs::jpeg::JpegEncoder;
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use serde_json::json;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;
use viper_client::video::media::{H264Depacketizer, RtpPacket};
use viper_client::video::{MediaKind, MediaPacket};
use webrtc_srtp::context::Context;
use webrtc_srtp::protection_profile::ProtectionProfile;

use crate::doorbell_audio::{AudioSender, CODEC_AAC_ELD, CODEC_OPUS};
use crate::viper::UnitSlot;

/// Shown when no snapshot could be captured yet.
pub const PLACEHOLDER_SNAPSHOT: &[u8] = include_bytes!("../assets/doorbell-placeholder.jpg");

/// How long a snapshot capture may take (call setup + first IDR frame).
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(8);
/// Upper bound of H.264 data collected while waiting for an IDR frame.
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
const JPEG_QUALITY: u8 = 80;
const NAL_SLICE: u8 = 1;
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
/// Frames decoded for a snapshot, starting from an IDR.
const SNAPSHOT_FRAMES: usize = 3;
/// How long the video keeps running after a snapshot, so that the next one
/// (the Home app asks every 10 seconds) comes from the running stream.
const SNAPSHOT_WARM: Duration = Duration::from_secs(30);

const FEED_QUEUE: usize = 512;
/// Sender reports as often as HomeKit asks for them (rtcp_interval 0.5),
/// the first one right after the first packet: a home hub relaying a remote
/// stream stopped it within a second, before any report.
pub(crate) const RTCP_INTERVAL: Duration = Duration::from_millis(500);
/// How often the stream statistics are logged.
const STATS_INTERVAL: Duration = Duration::from_secs(5);
/// Minimum delay between two key frames asked for HomeKit (PLI/FIR).
const KEY_FRAME_MIN_INTERVAL: Duration = Duration::from_secs(1);
/// A source timestamp jump larger than this is a new call: the outgoing
/// timestamps continue from the last one instead.
const MAX_TIMESTAMP_JUMP: u32 = 90_000 * 5;
/// Outgoing timestamp step used across such a jump (one frame at 30 fps).
const TIMESTAMP_STEP: u32 = 3_000;
const RTCP_SR: u8 = 200;
const RTCP_RR: u8 = 201;
const RTCP_RTPFB: u8 = 205;
const RTCP_PSFB: u8 = 206;
const PSFB_PLI: u8 = 1;
const PSFB_FIR: u8 = 4;
/// H.264 RTP payload type used when HomeKit does not say.
const DEFAULT_PAYLOAD_TYPE: u8 = 99;

#[derive(Debug, thiserror::Error)]
pub enum CameraError {
    #[error("the doorbell is not connected to the ViP system")]
    NotConnected,
    #[error("ViP: {0}")]
    Viper(String),
    #[error("no stream {0}")]
    UnknownStream(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("SRTP: {0}")]
    Srtp(String),
    #[error("snapshot: {0}")]
    Snapshot(String),
    #[error("audio: {0}")]
    Audio(String),
}

/// Shares the external unit video among its consumers (HomeKit streams,
/// snapshots): the media is requested from the ViP unit for the first one and
/// stopped when the last one goes away.
pub struct VideoFeed {
    slot: UnitSlot,
    sender: Mutex<Option<broadcast::Sender<Arc<MediaPacket>>>>,
}

impl VideoFeed {
    pub fn new(slot: UnitSlot) -> Arc<VideoFeed> {
        Arc::new(VideoFeed {
            slot,
            sender: Mutex::new(None),
        })
    }

    pub async fn subscribe(
        self: &Arc<Self>,
    ) -> Result<broadcast::Receiver<Arc<MediaPacket>>, CameraError> {
        let mut sender = self.sender.lock().await;
        if let Some(tx) = sender.as_ref() {
            let rx = tx.subscribe();
            // the unit only sends a key frame when the video starts
            self.request_key_frame().await;
            return Ok(rx);
        }
        let unit = self
            .slot
            .read()
            .await
            .clone()
            .ok_or(CameraError::NotConnected)?;
        let mut packets = unit
            .start_media(true)
            .await
            .map_err(|e| CameraError::Viper(e.to_string()))?;
        let (tx, rx) = broadcast::channel(FEED_QUEUE);
        *sender = Some(tx.clone());
        let feed: Weak<VideoFeed> = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(packet) = packets.recv().await {
                if tx.send(Arc::new(packet)).is_err() {
                    debug!("Nobody watches the doorbell any more");
                    break;
                }
            }
            drop(tx);
            if let Some(feed) = feed.upgrade() {
                // dropping the last sender ends the subscriptions
                *feed.sender.lock().await = None;
            }
            if let Err(e) = unit.stop_media().await {
                debug!("Cannot stop the doorbell media: {e}");
            }
        });
        Ok(rx)
    }
}

/// JPEG snapshots of the external unit, cached for `max_age`.
pub struct SnapshotSource {
    feed: Arc<VideoFeed>,
    max_age: Duration,
    cache: Mutex<Option<(Instant, Vec<u8>)>>,
    /// Set by a ring: the cached snapshot must not be reused.
    stale: AtomicBool,
    /// Keeps the video running for [`SNAPSHOT_WARM`] after a snapshot.
    warm: Mutex<Option<JoinHandle<()>>>,
}

impl SnapshotSource {
    pub fn new(feed: Arc<VideoFeed>, max_age: Duration) -> Arc<SnapshotSource> {
        Arc::new(SnapshotSource {
            feed,
            max_age,
            cache: Mutex::new(None),
            stale: AtomicBool::new(false),
            warm: Mutex::new(None),
        })
    }

    /// Someone rings: the next snapshot must show who it is.
    pub fn invalidate(&self) {
        self.stale.store(true, Ordering::Relaxed);
    }

    /// A recent snapshot, captured if the cached one is too old; the last
    /// one (or a placeholder) if capturing fails.
    pub async fn snapshot(&self) -> Vec<u8> {
        // the lock also serializes captures
        let mut cache = self.cache.lock().await;
        let stale = self.stale.swap(false, Ordering::Relaxed);
        if let Some((at, jpeg)) = cache.as_ref()
            && !stale
            && at.elapsed() < self.max_age
        {
            return jpeg.clone();
        }
        match tokio::time::timeout(CAPTURE_TIMEOUT, self.capture()).await {
            Ok(Ok(jpeg)) => {
                debug!("Doorbell snapshot captured ({} bytes)", jpeg.len());
                *cache = Some((Instant::now(), jpeg.clone()));
                jpeg
            }
            Ok(Err(e)) => {
                warn!("Cannot capture a doorbell snapshot: {e}");
                cached_or_placeholder(&cache)
            }
            Err(_) => {
                warn!("Doorbell snapshot capture timed out");
                cached_or_placeholder(&cache)
            }
        }
    }

    async fn capture(&self) -> Result<Vec<u8>, CameraError> {
        let mut packets = self.feed.subscribe().await?;
        let mut frame = FrameCollector::default();
        loop {
            let packet = match packets.recv().await {
                Ok(packet) => packet,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(CameraError::Snapshot("the video ended".into()));
                }
            };
            if packet.kind != MediaKind::Video {
                continue;
            }
            if let Some(stream) = frame.push(&packet.rtp)? {
                // OpenH264 is not Send: decode on a blocking thread
                match tokio::task::spawn_blocking(move || decode_to_jpeg(&stream))
                    .await
                    .map_err(|e| CameraError::Snapshot(e.to_string()))?
                {
                    Ok(jpeg) => {
                        self.keep_warm(packets).await;
                        return Ok(jpeg);
                    }
                    // a damaged frame: wait for the next IDR
                    Err(e) => debug!("Snapshot frame not decoded: {e}"),
                }
            }
        }
    }
}

impl SnapshotSource {
    /// Holds the video subscription for a while, so that the external unit
    /// keeps streaming for the next snapshot.
    async fn keep_warm(&self, packets: broadcast::Receiver<Arc<MediaPacket>>) {
        let keeper = tokio::spawn(async move {
            tokio::time::sleep(SNAPSHOT_WARM).await;
            drop(packets);
        });
        if let Some(previous) = self.warm.lock().await.replace(keeper) {
            previous.abort();
        }
    }
}

fn cached_or_placeholder(cache: &Option<(Instant, Vec<u8>)>) -> Vec<u8> {
    cache
        .as_ref()
        .map(|(_, jpeg)| jpeg.clone())
        .unwrap_or_else(|| PLACEHOLDER_SNAPSHOT.to_vec())
}

/// Collects an Annex B H.264 stream from RTP packets, from an SPS to the
/// `SNAPSHOT_FRAMES`th picture from an IDR (the external unit sends one
/// slice per picture): the first picture after the camera turns on is
/// black. A lost packet restarts the collection from the next SPS.
#[derive(Default)]
struct FrameCollector {
    depacketizer: H264Depacketizer,
    stream: Vec<u8>,
    has_idr: bool,
    frames: usize,
    last_sequence: Option<u16>,
}

impl FrameCollector {
    fn push(&mut self, rtp: &[u8]) -> Result<Option<Vec<u8>>, CameraError> {
        let Some(packet) = RtpPacket::parse(rtp) else {
            return Ok(None);
        };
        if self
            .last_sequence
            .is_some_and(|last| packet.sequence != last.wrapping_add(1))
        {
            self.depacketizer.reset();
            self.stream.clear();
            self.has_idr = false;
            self.frames = 0;
        }
        self.last_sequence = Some(packet.sequence);
        for nal in self.depacketizer.push(packet.payload) {
            let nal_type = nal.first().map_or(0, |h| h & 0x1f);
            if self.stream.is_empty() && nal_type != NAL_SPS {
                continue;
            }
            self.has_idr |= nal_type == NAL_IDR;
            self.stream.extend_from_slice(&[0, 0, 0, 1]);
            self.stream.extend_from_slice(&nal);
            if self.has_idr && (nal_type == NAL_IDR || nal_type == NAL_SLICE) {
                self.frames += 1;
                if self.frames >= SNAPSHOT_FRAMES {
                    return Ok(Some(std::mem::take(&mut self.stream)));
                }
            }
        }
        if self.stream.len() > MAX_CAPTURE_BYTES {
            return Err(CameraError::Snapshot("no IDR frame in the video".into()));
        }
        Ok(None)
    }
}

/// Decodes the last picture of an Annex B stream and encodes it as JPEG.
fn decode_to_jpeg(stream: &[u8]) -> Result<Vec<u8>, CameraError> {
    let error = |e: &dyn std::fmt::Display| CameraError::Snapshot(e.to_string());
    let mut decoder = Decoder::new().map_err(|e| error(&e))?;
    let mut picture = None;
    for nal in nal_units(stream) {
        let unit = [&[0, 0, 0, 1][..], nal].concat();
        match decoder.decode(&unit) {
            Ok(Some(yuv)) => {
                let (width, height) = yuv.dimensions();
                let mut rgb = vec![0u8; width * height * 3];
                yuv.write_rgb8(&mut rgb);
                picture = Some((rgb, width as u32, height as u32));
            }
            Ok(None) => {}
            // a damaged NAL unit (lost packets): the picture may still decode
            Err(e) => debug!("H.264 NAL unit not decoded: {e}"),
        }
    }
    if picture.is_none() {
        // the decoder may keep the last picture until it is flushed
        if let Some(yuv) = decoder
            .flush_remaining()
            .map_err(|e| error(&e))?
            .into_iter()
            .last()
        {
            let (width, height) = yuv.dimensions();
            let mut rgb = vec![0u8; width * height * 3];
            yuv.write_rgb8(&mut rgb);
            picture = Some((rgb, width as u32, height as u32));
        }
    }
    let (rgb, width, height) =
        picture.ok_or_else(|| CameraError::Snapshot("no picture decoded".into()))?;
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(&rgb, width, height, ExtendedColorType::Rgb8)
        .map_err(|e| error(&e))?;
    Ok(jpeg)
}

/// The NAL units of an Annex B stream (3 or 4 byte start codes), without
/// their start code.
fn nal_units(stream: &[u8]) -> Vec<&[u8]> {
    // (start code position, payload position) of every NAL unit
    let mut bounds = vec![];
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i..i + 3] == [0, 0, 1] {
            let start = if i > 0 && stream[i - 1] == 0 {
                i - 1
            } else {
                i
            };
            bounds.push((start, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    bounds
        .iter()
        .enumerate()
        .map(|(n, &(_, payload))| {
            let end = bounds.get(n + 1).map_or(stream.len(), |&(next, _)| next);
            &stream[payload..end]
        })
        .collect()
}

fn max_bitrate(stream: &StreamInfo) -> Option<u32> {
    stream
        .video_rtp
        .as_ref()
        .and_then(|rtp| rtp.max_bitrate)
        .map(u32::from)
        .filter(|&kbps| kbps > 0)
}

impl VideoFeed {
    /// Follows the bitrate HomeKit asks for: the external unit always sends
    /// 320x240, HomeKit stalls on more than it asked. Also brings a key
    /// frame.
    async fn set_bitrate(&self, kbps: u32) {
        let unit = self.slot.read().await.clone();
        if let Some(unit) = unit
            && let Err(e) = unit.set_video_bitrate(kbps).await
        {
            debug!("Cannot change the video bitrate: {e}");
        }
    }

    async fn request_key_frame(&self) {
        let unit = self.slot.read().await.clone();
        if let Some(unit) = unit
            && let Err(e) = unit.request_key_frame().await
        {
            debug!("Cannot request a key frame: {e}");
        }
    }
}

/// Video and audio tasks of each HomeKit stream.
type StreamTasks = HashMap<(Uuid, usize), (Vec<JoinHandle<()>>, Arc<StreamSummary>)>;

/// What a HomeKit stream got, logged when HomeKit stops it: the periodic
/// statistics miss the streams stopped within seconds.
pub(crate) struct StreamSummary {
    started: Instant,
    first_video: OnceLock<Instant>,
    first_rtcp: OnceLock<Instant>,
    video_packets: AtomicU32,
    audio_packets: AtomicU32,
    rtcp_packets: AtomicU32,
}

impl StreamSummary {
    fn new() -> StreamSummary {
        StreamSummary {
            started: Instant::now(),
            first_video: OnceLock::new(),
            first_rtcp: OnceLock::new(),
            video_packets: AtomicU32::new(0),
            audio_packets: AtomicU32::new(0),
            rtcp_packets: AtomicU32::new(0),
        }
    }

    fn video_sent(&self) {
        self.first_video.get_or_init(Instant::now);
        self.video_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn audio_sent(&self) {
        self.audio_packets.fetch_add(1, Ordering::Relaxed);
    }

    fn rtcp_received(&self) {
        self.first_rtcp.get_or_init(Instant::now);
        self.rtcp_packets.fetch_add(1, Ordering::Relaxed);
    }

    fn describe(&self) -> String {
        let after = |at: &OnceLock<Instant>| match at.get() {
            Some(at) => format!("after {} ms", at.duration_since(self.started).as_millis()),
            None => "never".to_string(),
        };
        format!(
            "lasted {} ms, {} video packets (first {}), {} audio packets, {} RTCP packets from HomeKit (first {})",
            self.started.elapsed().as_millis(),
            self.video_packets.load(Ordering::Relaxed),
            after(&self.first_video),
            self.audio_packets.load(Ordering::Relaxed),
            self.rtcp_packets.load(Ordering::Relaxed),
            after(&self.first_rtcp),
        )
    }
}

/// [`MediaProvider`] streaming the ViP video and audio to HomeKit.
pub struct ViperMediaProvider {
    address: IpAddr,
    options: StreamOptions,
    feed: Arc<VideoFeed>,
    streams: Mutex<StreamTasks>,
    /// Bitrate asked to the external unit when no HomeKit stream runs
    /// (snapshots).
    idle_kbps: u32,
}

impl ViperMediaProvider {
    pub fn new(address: Ipv4Addr, feed: Arc<VideoFeed>, idle_kbps: u32) -> ViperMediaProvider {
        ViperMediaProvider {
            address: IpAddr::V4(address),
            options: stream_options(),
            feed,
            streams: Mutex::new(HashMap::new()),
            idle_kbps,
        }
    }
}

/// What we tell HomeKit we can stream. The unit sends 320x240 whatever is
/// asked, and iOS decodes the resolution of the stream itself; the usual
/// sizes are listed so that every HomeKit client finds one it likes.
fn stream_options() -> StreamOptions {
    StreamOptions {
        video: json!({
            "resolutions": [
                [320, 240, 15], [320, 240, 30], [480, 360, 30],
                [640, 360, 30], [640, 480, 30], [1280, 720, 30]
            ],
            "codec": { "profiles": [0, 1, 2], "levels": [0, 1, 2] }
        }),
        // the external unit audio is encoded to AAC-ELD or Opus
        audio: json!({
            "codecs": [
                { "type": "AAC-eld", "samplerate": 16 },
                { "type": "OPUS", "samplerate": 24 },
                { "type": "OPUS", "samplerate": 16 }
            ]
        }),
        srtp: true,
    }
}

#[async_trait]
impl MediaProvider for ViperMediaProvider {
    type Error = CameraError;

    fn address(&self, _idx: usize) -> Result<&IpAddr, CameraError> {
        Ok(&self.address)
    }

    fn options(&self, _idx: usize) -> Result<&StreamOptions, CameraError> {
        Ok(&self.options)
    }

    async fn start(
        &self,
        idx: usize,
        stream: StreamInfo,
        session: &SessionState,
    ) -> Result<(), CameraError> {
        let payload_type = stream
            .video_rtp
            .as_ref()
            .and_then(|rtp| rtp.payload_type.as_ref())
            .and_then(|pt| pt.first().copied())
            .unwrap_or(DEFAULT_PAYLOAD_TYPE);
        let target = SocketAddr::new(session.address, session.video_port);
        info!(
            "HomeKit asks the doorbell video ({:?}, {:?}) for {target}",
            stream.video_attributes, stream.video_rtp
        );
        // the bitrate first: a running video is at the snapshot bitrate,
        // a burst a remote HomeKit stream does not survive. The packets go
        // out from the key frame the new MEDIAREQ brings (iOS cannot decode
        // anything before one anyway)
        if let Some(kbps) = max_bitrate(&stream) {
            self.feed.set_bitrate(kbps).await;
        }
        let packets = self.feed.subscribe().await?;
        let socket = bind_rtp_socket(session.video_port).await?;
        socket.connect(target).await?;
        let mut sender = RtpSender::new(session, payload_type)?;
        let feed = self.feed.clone();
        let summary = Arc::new(StreamSummary::new());
        let video_summary = summary.clone();
        let mut tasks = vec![tokio::spawn(async move {
            if let Err(e) = sender.forward(packets, socket, feed, &video_summary).await {
                warn!("Doorbell video stream stopped: {e}");
            }
        })];
        match stream.audio.as_ref() {
            Some(audio) if matches!(audio.codec.first(), Some(&(CODEC_OPUS | CODEC_AAC_ELD))) => {
                let codec = if audio.codec.first() == Some(&CODEC_OPUS) {
                    "Opus"
                } else {
                    "AAC-ELD"
                };
                info!(
                    "HomeKit asks the doorbell audio ({codec} {} kHz, {} ms, {} kbit/s)",
                    audio.sample_rate, audio.packet_time, audio.max_bitrate
                );
                let mut sender = AudioSender::new(session, audio)?;
                let packets = self.feed.subscribe().await?;
                let socket = bind_rtp_socket(session.audio_port).await?;
                socket
                    .connect(SocketAddr::new(session.address, session.audio_port))
                    .await?;
                let audio_summary = summary.clone();
                tasks.push(tokio::spawn(async move {
                    if let Err(e) = sender.forward(packets, socket, &audio_summary).await {
                        warn!("Doorbell audio stream stopped: {e}");
                    }
                }));
            }
            Some(audio) => warn!("Doorbell audio codec {:?} not supported", audio.codec),
            None => {}
        }
        if let Some(previous) = self
            .streams
            .lock()
            .await
            .insert((session.session_id, idx), (tasks, summary))
        {
            previous.0.iter().for_each(JoinHandle::abort);
        }
        Ok(())
    }

    async fn reconfigure(
        &self,
        idx: usize,
        stream: StreamInfo,
        _session: &SessionState,
    ) -> Result<(), CameraError> {
        // the external unit has a single resolution, only the bitrate follows
        info!(
            "HomeKit reconfigures stream {idx}: {:?}, {:?}",
            stream.video_attributes, stream.video_rtp
        );
        if let Some(kbps) = max_bitrate(&stream) {
            self.feed.set_bitrate(kbps).await;
        }
        Ok(())
    }

    async fn stop(&self, idx: usize, session: &SessionState) -> Result<(), CameraError> {
        let mut streams = self.streams.lock().await;
        let (tasks, summary) = streams
            .remove(&(session.session_id, idx))
            .ok_or(CameraError::UnknownStream(idx))?;
        tasks.iter().for_each(JoinHandle::abort);
        info!("HomeKit stopped the doorbell video: {}", summary.describe());
        if streams.is_empty() {
            // back to the snapshot quality
            self.feed.set_bitrate(self.idle_kbps).await;
        }
        Ok(())
    }
}

/// Binds the port we told iOS we use (the same as its own, as the setup
/// response echoes it), so its RTCP reports reach us; any port otherwise.
async fn bind_rtp_socket(port: u16) -> Result<UdpSocket, CameraError> {
    match UdpSocket::bind(("0.0.0.0", port)).await {
        Ok(socket) => Ok(socket),
        Err(_) => Ok(UdpSocket::bind(("0.0.0.0", 0)).await?),
    }
}

/// Rewrites the external unit RTP packets for one HomeKit session and
/// encrypts them.
struct RtpSender {
    srtp: Context,
    /// Decrypts the RTCP of the HomeKit device (same keys both ways).
    srtcp_in: Context,
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    last_timestamp: u32,
    /// Added to the source timestamps, to keep them continuous.
    timestamp_offset: u32,
    last_source_timestamp: Option<u32>,
    packets: u32,
    octets: u32,
    stats: StreamStats,
    /// Nothing goes out before the first key frame.
    key_frame_seen: bool,
}

/// What happened since the stats were last logged.
#[derive(Default)]
struct StreamStats {
    packets: u32,
    octets: u64,
    send_errors: u32,
    key_frame_requests: u32,
    nacks: u32,
    /// Last receiver report: fraction lost (/256), cumulative lost (signed,
    /// duplicates count as negative), jitter.
    report: Option<(u8, i32, u32)>,
}

impl RtpSender {
    fn new(session: &SessionState, payload_type: u8) -> Result<RtpSender, CameraError> {
        let crypto = &session.video_crypto;
        let srtp = Context::new(
            &crypto.master_key,
            &crypto.master_salt,
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .map_err(|e| CameraError::Srtp(e.to_string()))?;
        let srtcp_in = Context::new(
            &crypto.master_key,
            &crypto.master_salt,
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .map_err(|e| CameraError::Srtp(e.to_string()))?;
        Ok(RtpSender {
            srtp,
            srtcp_in,
            payload_type,
            ssrc: session.video_ssrc,
            sequence: rand::random(),
            last_timestamp: 0,
            timestamp_offset: 0,
            last_source_timestamp: None,
            packets: 0,
            octets: 0,
            stats: StreamStats::default(),
            key_frame_seen: false,
        })
    }

    async fn forward(
        &mut self,
        mut packets: broadcast::Receiver<Arc<MediaPacket>>,
        socket: UdpSocket,
        feed: Arc<VideoFeed>,
        summary: &StreamSummary,
    ) -> Result<(), CameraError> {
        let mut report = tokio::time::interval(RTCP_INTERVAL);
        let mut stats = tokio::time::interval(STATS_INTERVAL);
        let mut buf = vec![0u8; 2048];
        let mut last_key_frame: Option<Instant> = None;
        loop {
            tokio::select! {
                packet = packets.recv() => match packet {
                    Ok(packet) if packet.kind == MediaKind::Video => {
                        if !self.key_frame_seen {
                            if !starts_key_frame(&packet.rtp) {
                                continue;
                            }
                            self.key_frame_seen = true;
                        }
                        let Some(rtp) = self.rewrite(&packet.rtp) else {
                            continue;
                        };
                        let encrypted = self
                            .srtp
                            .encrypt_rtp(&rtp)
                            .map_err(|e| CameraError::Srtp(e.to_string()))?;
                        // ICMP errors from a port closed for a moment must
                        // not end the stream: HomeKit stops it when it wants
                        match socket.send(&encrypted).await {
                            Ok(_) if self.packets == 1 => {
                                summary.video_sent();
                                self.send_report(&socket).await?;
                                report.reset();
                            }
                            Ok(_) => summary.video_sent(),
                            Err(e) => {
                                self.stats.send_errors += 1;
                                debug!("Cannot send RTP: {e}");
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!("Doorbell video: {n} packets dropped");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("The doorbell video ended");
                        return Ok(());
                    }
                },
                received = socket.recv(&mut buf) => {
                    let Ok(n) = received else { continue };
                    summary.rtcp_received();
                    if self.on_rtcp(&buf[..n]) {
                        self.stats.key_frame_requests += 1;
                        let now = Instant::now();
                        if last_key_frame.is_none_or(|at| now.duration_since(at) >= KEY_FRAME_MIN_INTERVAL) {
                            last_key_frame = Some(now);
                            feed.request_key_frame().await;
                        }
                    }
                }
                _ = report.tick() => {
                    if self.packets == 0 {
                        continue;
                    }
                    self.send_report(&socket).await?;
                }
                _ = stats.tick() => self.log_stats(),
            }
        }
    }

    async fn send_report(&mut self, socket: &UdpSocket) -> Result<(), CameraError> {
        let sr = sender_report(self.ssrc, self.last_timestamp, self.packets, self.octets);
        let encrypted = self
            .srtp
            .encrypt_rtcp(&sr)
            .map_err(|e| CameraError::Srtp(e.to_string()))?;
        if let Err(e) = socket.send(&encrypted).await {
            debug!("Cannot send RTCP: {e}");
        }
        Ok(())
    }

    fn log_stats(&mut self) {
        let stats = std::mem::take(&mut self.stats);
        if stats.packets == 0 && stats.key_frame_requests == 0 && stats.report.is_none() {
            return;
        }
        let kbps = stats.octets * 8 / STATS_INTERVAL.as_secs() / 1000;
        let report = stats
            .report
            .map(|(fraction, lost, jitter)| {
                format!(
                    ", receiver lost {:.1}% ({lost} in total), jitter {jitter}",
                    fraction as f32 * 100.0 / 256.0
                )
            })
            .unwrap_or_default();
        info!(
            "Doorbell video: {} packets, {kbps} kbit/s, {} send errors, {} key frame requests, {} NACKs{report}",
            stats.packets, stats.send_errors, stats.key_frame_requests, stats.nacks
        );
    }

    /// Reads an RTCP packet of the HomeKit device; true when it asks for a
    /// key frame.
    fn on_rtcp(&mut self, packet: &[u8]) -> bool {
        // rtcp-mux: only RTCP is expected here (payload types 200-206)
        if packet.len() < 8 || packet[0] >> 6 != 2 || !(RTCP_SR..=RTCP_PSFB).contains(&packet[1]) {
            return false;
        }
        let plain = match self.srtcp_in.decrypt_rtcp(packet) {
            Ok(plain) => plain,
            Err(e) => {
                debug!("Cannot decrypt the HomeKit RTCP: {e}");
                return false;
            }
        };
        let mut key_frame = false;
        let mut rest: &[u8] = &plain;
        while rest.len() >= 4 {
            let count = rest[0] & 0x1f;
            let kind = rest[1];
            let len = (u16::from_be_bytes([rest[2], rest[3]]) as usize + 1) * 4;
            if len > rest.len() {
                break;
            }
            let body = &rest[4..len];
            match kind {
                RTCP_SR | RTCP_RR => {
                    // report blocks follow the sender SSRC (and sender info)
                    let blocks = if kind == RTCP_SR { 24 } else { 4 };
                    if count > 0 && body.len() >= blocks + 24 {
                        let b = &body[blocks..blocks + 24];
                        // a signed 24 bit value
                        let lost = i32::from_be_bytes([b[5], b[6], b[7], 0]) >> 8;
                        let jitter = u32::from_be_bytes([b[12], b[13], b[14], b[15]]);
                        self.stats.report = Some((b[4], lost, jitter));
                    }
                }
                RTCP_PSFB if count == PSFB_PLI || count == PSFB_FIR => key_frame = true,
                RTCP_RTPFB => self.stats.nacks += 1,
                other => debug!("HomeKit RTCP {other}/{count} ignored"),
            }
            rest = &rest[len..];
        }
        key_frame
    }

    /// The packet with HomeKit's payload type, our SSRC and a continuous
    /// sequence; `None` if it is not RTP.
    fn rewrite(&mut self, rtp: &[u8]) -> Option<Vec<u8>> {
        if rtp.len() < 12 || rtp[0] >> 6 != 2 {
            return None;
        }
        let mut out = rtp.to_vec();
        out[1] = (rtp[1] & 0x80) | (self.payload_type & 0x7f);
        out[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        out[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        let source = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
        if let Some(last) = self.last_source_timestamp {
            let jump = source.wrapping_sub(last);
            // a new call restarts the clock of the external unit
            if jump > MAX_TIMESTAMP_JUMP && jump.wrapping_neg() > MAX_TIMESTAMP_JUMP {
                self.timestamp_offset = self
                    .last_timestamp
                    .wrapping_add(TIMESTAMP_STEP)
                    .wrapping_sub(source);
            }
        }
        self.last_source_timestamp = Some(source);
        let timestamp = source.wrapping_add(self.timestamp_offset);
        out[4..8].copy_from_slice(&timestamp.to_be_bytes());
        self.sequence = self.sequence.wrapping_add(1);
        self.last_timestamp = timestamp;
        self.packets = self.packets.wrapping_add(1);
        self.octets = self.octets.wrapping_add((rtp.len() - 12) as u32);
        self.stats.packets += 1;
        self.stats.octets += (rtp.len() - 12) as u64;
        Some(out)
    }
}

/// Whether an H.264 RTP packet (RFC 6184) starts a key frame: an SPS or an
/// IDR slice, alone, first in a STAP-A or in the first FU-A fragment.
fn starts_key_frame(rtp: &[u8]) -> bool {
    const STAP_A: u8 = 24;
    const FU_A: u8 = 28;
    let Some(packet) = RtpPacket::parse(rtp) else {
        return false;
    };
    let payload = packet.payload;
    let key = |nal_type: u8| nal_type == NAL_SPS || nal_type == NAL_IDR;
    match payload.first().map(|h| h & 0x1f) {
        Some(STAP_A) => payload.get(3).is_some_and(|h| key(h & 0x1f)),
        Some(FU_A) => payload
            .get(1)
            .is_some_and(|h| h & 0x80 != 0 && key(h & 0x1f)),
        Some(nal_type) => key(nal_type),
        None => false,
    }
}

/// RTCP sender report (RFC 3550 §6.4.1) without reception blocks.
pub(crate) fn sender_report(ssrc: u32, rtp_timestamp: u32, packets: u32, octets: u32) -> Vec<u8> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // NTP epoch is 1900-01-01
    let ntp_seconds = (now.as_secs() + 2_208_988_800) as u32;
    let ntp_fraction = ((now.subsec_nanos() as u64) << 32) / 1_000_000_000;
    let mut sr = vec![0x80, 200, 0x00, 0x06];
    sr.extend_from_slice(&ssrc.to_be_bytes());
    sr.extend_from_slice(&ntp_seconds.to_be_bytes());
    sr.extend_from_slice(&(ntp_fraction as u32).to_be_bytes());
    sr.extend_from_slice(&rtp_timestamp.to_be_bytes());
    sr.extend_from_slice(&packets.to_be_bytes());
    sr.extend_from_slice(&octets.to_be_bytes());
    sr
}

#[cfg(test)]
mod tests {
    use super::*;
    use hap::accessory::camera::protocol::SetupEndpointCrypto;

    fn session() -> SessionState {
        let crypto = SetupEndpointCrypto {
            cypto_suite: 0,
            master_key: vec![7; 16],
            master_salt: vec![9; 14],
        };
        SessionState {
            session_id: Uuid::nil(),
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            video_port: 0,
            audio_port: 0,
            video_crypto: crypto.clone(),
            audio_crypto: crypto,
            video_ssrc: 0xdead_beef,
            audio_ssrc: 1,
        }
    }

    fn device_packet(seq: u16, marker: bool) -> Vec<u8> {
        let mut rtp = vec![0x80, 99 | if marker { 0x80 } else { 0 }];
        rtp.extend_from_slice(&seq.to_be_bytes());
        rtp.extend_from_slice(&0x0102_0304u32.to_be_bytes());
        rtp.extend_from_slice(&0xc080_9a0du32.to_be_bytes());
        rtp.extend_from_slice(&[0x65, 1, 2, 3]);
        rtp
    }

    #[test]
    fn finds_the_start_of_key_frames() {
        let with_payload = |payload: &[u8]| {
            let mut rtp = device_packet(1, false);
            rtp.truncate(12);
            rtp.extend_from_slice(payload);
            rtp
        };
        // single NAL units: IDR, SPS, then a P slice
        assert!(starts_key_frame(&with_payload(&[0x65, 1])));
        assert!(starts_key_frame(&with_payload(&[0x67, 1])));
        assert!(!starts_key_frame(&with_payload(&[0x41, 1])));
        // STAP-A carrying SPS first
        assert!(starts_key_frame(&with_payload(&[0x78, 0, 2, 0x67, 1])));
        // FU-A of an IDR: the first fragment only
        assert!(starts_key_frame(&with_payload(&[0x7c, 0x85, 1])));
        assert!(!starts_key_frame(&with_payload(&[0x7c, 0x05, 1])));
        assert!(!starts_key_frame(&with_payload(&[0x7c, 0x81, 1])));
        assert!(!starts_key_frame(&[0x80]));
    }

    #[test]
    fn rewrites_payload_type_ssrc_and_sequence() {
        let mut sender = RtpSender::new(&session(), 96).unwrap();
        let first = sender.rewrite(&device_packet(500, true)).unwrap();
        let second = sender.rewrite(&device_packet(900, false)).unwrap();

        assert_eq!(first[1], 0x80 | 96, "marker kept, payload type replaced");
        assert_eq!(second[1], 96);
        let seq = |p: &[u8]| u16::from_be_bytes([p[2], p[3]]);
        assert_eq!(seq(&second), seq(&first).wrapping_add(1));
        assert_eq!(&first[8..12], &0xdead_beefu32.to_be_bytes());
        assert_eq!(
            &first[4..8],
            &0x0102_0304u32.to_be_bytes(),
            "timestamp kept"
        );
        assert_eq!(&first[12..], &[0x65, 1, 2, 3]);
        assert_eq!((sender.packets, sender.octets), (2, 8));
        assert!(sender.rewrite(&[0u8; 20]).is_none());
    }

    #[test]
    fn encrypts_with_the_negotiated_keys() {
        let mut sender = RtpSender::new(&session(), 99).unwrap();
        let rtp = sender.rewrite(&device_packet(1, true)).unwrap();
        let encrypted = sender.srtp.encrypt_rtp(&rtp).unwrap();
        // header in clear, payload encrypted, 10 byte auth tag appended
        assert_eq!(&encrypted[..12], &rtp[..12]);
        assert_ne!(&encrypted[12..16], &rtp[12..16]);
        assert_eq!(encrypted.len(), rtp.len() + 10);

        let mut receiver = Context::new(
            &[7; 16],
            &[9; 14],
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .unwrap();
        assert_eq!(&receiver.decrypt_rtp(&encrypted).unwrap()[..], &rtp[..]);

        let sr = sender_report(sender.ssrc, 1234, 1, 4);
        let encrypted = sender.srtp.encrypt_rtcp(&sr).unwrap();
        assert_eq!(&receiver.decrypt_rtcp(&encrypted).unwrap()[..], &sr[..]);
    }

    #[test]
    fn keeps_timestamps_continuous_across_calls() {
        let mut sender = RtpSender::new(&session(), 99).unwrap();
        let with_timestamp = |ts: u32| {
            let mut p = device_packet(1, true);
            p[4..8].copy_from_slice(&ts.to_be_bytes());
            p
        };
        let ts = |p: &[u8]| u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
        let first = sender.rewrite(&with_timestamp(1_000_000)).unwrap();
        let next = sender.rewrite(&with_timestamp(1_006_000)).unwrap();
        assert_eq!(ts(&next) - ts(&first), 6_000, "normal steps kept");
        // a new call: the external unit clock starts somewhere else
        let restarted = sender.rewrite(&with_timestamp(42)).unwrap();
        assert_eq!(ts(&restarted), ts(&next) + TIMESTAMP_STEP);
        let after = sender.rewrite(&with_timestamp(42 + 6_000)).unwrap();
        assert_eq!(ts(&after), ts(&restarted) + 6_000);
    }

    #[test]
    fn reads_key_frame_requests_and_reports_from_homekit() {
        let mut sender = RtpSender::new(&session(), 99).unwrap();
        let mut device = Context::new(
            &[7; 16],
            &[9; 14],
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .unwrap();
        // receiver report (one block, 25% lost) + PLI, as iOS sends them
        let mut rr = vec![0x81, RTCP_RR, 0x00, 0x07];
        rr.extend_from_slice(&0x1111_1111u32.to_be_bytes());
        rr.extend_from_slice(&0xdead_beefu32.to_be_bytes());
        rr.extend_from_slice(&[64, 0, 0, 12]);
        rr.extend_from_slice(&[0; 4]);
        rr.extend_from_slice(&30u32.to_be_bytes());
        rr.extend_from_slice(&[0; 8]);
        let mut pli = vec![0x80 | PSFB_PLI, RTCP_PSFB, 0x00, 0x02];
        pli.extend_from_slice(&0x1111_1111u32.to_be_bytes());
        pli.extend_from_slice(&0xdead_beefu32.to_be_bytes());

        let report_only = device.encrypt_rtcp(&rr).unwrap();
        assert!(!sender.on_rtcp(&report_only));
        assert_eq!(sender.stats.report, Some((64, 12, 30)));

        let compound = device.encrypt_rtcp(&[rr, pli].concat()).unwrap();
        assert!(sender.on_rtcp(&compound));
        // RTP that loops back is not RTCP
        assert!(!sender.on_rtcp(&device_packet(1, true)));
    }

    #[test]
    fn sender_report_layout() {
        let sr = sender_report(0x0102_0304, 0xaabb_ccdd, 5, 600);
        assert_eq!(sr.len(), 28);
        assert_eq!(&sr[..4], &[0x80, 200, 0x00, 0x06]);
        assert_eq!(&sr[4..8], &[1, 2, 3, 4]);
        assert_eq!(&sr[16..20], &0xaabb_ccddu32.to_be_bytes());
        assert_eq!(&sr[20..24], &5u32.to_be_bytes());
        assert_eq!(&sr[24..28], &600u32.to_be_bytes());
    }

    #[test]
    fn placeholder_is_a_jpeg() {
        assert_eq!(&PLACEHOLDER_SNAPSHOT[..2], &[0xff, 0xd8]);
    }

    /// Three 320x240 Baseline frames generated with ffmpeg's testsrc.
    const TEST_STREAM: &[u8] = include_bytes!("../assets/test-baseline.h264");

    fn jpeg_size(jpeg: &[u8]) -> (u32, u32) {
        let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).unwrap();
        (image.width(), image.height())
    }

    #[test]
    fn decodes_an_idr_frame_to_jpeg() {
        let jpeg = decode_to_jpeg(TEST_STREAM).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert_eq!(jpeg_size(&jpeg), (320, 240));
    }

    #[test]
    fn collects_rtp_packets_from_an_idr_frame() {
        // one RTP packet per NAL unit (start code stripped), marker on the
        // last NAL of each picture
        let nals = nal_units(TEST_STREAM);
        let mut collector = FrameCollector::default();
        let mut stream = None;
        for (seq, nal) in nals.iter().enumerate() {
            let nal_type = nal[0] & 0x1f;
            let marker = nal_type == 1 || nal_type == NAL_IDR;
            let mut rtp = vec![0x80, if marker { 0x80 | 99 } else { 99 }];
            rtp.extend_from_slice(&(seq as u16).to_be_bytes());
            rtp.extend_from_slice(&[0; 8]);
            rtp.extend_from_slice(nal);
            if let Some(frame) = collector.push(&rtp).unwrap() {
                stream = Some(frame);
                break;
            }
        }
        let stream = stream.expect("no complete IDR frame");
        let jpeg = decode_to_jpeg(&stream).unwrap();
        assert_eq!(jpeg_size(&jpeg), (320, 240));
    }

    #[test]
    fn splits_annex_b_nal_units() {
        let stream = [0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 3, 0, 0, 0, 1, 0x65];
        let nals = nal_units(&stream);
        assert_eq!(nals, vec![&[0x67, 1][..], &[0x68, 2, 3][..], &[0x65][..]]);
    }

    /// Snapshot through the whole path against the real ViP system:
    /// `ICONA_IP=… ICONA_TOKEN=… VIPER_ENTRANCE=… cargo test -p comelit-hub-hap snapshot_from -- --ignored`
    /// (turns the external unit camera on for a few seconds).
    #[tokio::test]
    #[ignore]
    async fn snapshot_from_the_real_door_unit() {
        let ip = std::env::var("ICONA_IP").expect("ICONA_IP not set");
        let token = std::env::var("ICONA_TOKEN").expect("ICONA_TOKEN not set");
        let entrance = std::env::var("VIPER_ENTRANCE").expect("VIPER_ENTRANCE not set");
        let client = viper_client::ViperClient::connect(&ip, viper_client::ICONA_BRIDGE_PORT)
            .await
            .unwrap();
        let (client, vip) = tokio::task::spawn_blocking(move || {
            let mut client = client;
            client.authorize(&token).unwrap();
            let config = client.configuration("all").unwrap();
            (client, config.vip)
        })
        .await
        .unwrap();
        let (unit, _events, task) = client
            .into_unit(
                &vip,
                Some(&entrance),
                Duration::from_secs(30),
                viper_client::video::VideoParams::default(),
            )
            .unwrap();
        let slot = UnitSlot::default();
        *slot.write().await = Some(unit.clone());
        // let the tunnel open before asking for media
        tokio::time::sleep(Duration::from_secs(1)).await;

        let snapshots = SnapshotSource::new(VideoFeed::new(slot), Duration::from_secs(60));
        let started = Instant::now();
        let jpeg = snapshots.snapshot().await;
        assert_ne!(jpeg, PLACEHOLDER_SNAPSHOT, "capture failed");
        println!(
            "snapshot {:?} in {:?}, {} bytes",
            jpeg_size(&jpeg),
            started.elapsed(),
            jpeg.len()
        );
        let cached = snapshots.snapshot().await;
        assert_eq!(cached, jpeg, "second request served from the cache");
        if let Ok(out) = std::env::var("DOORBELL_JPEG") {
            std::fs::write(out, &jpeg).unwrap();
        }
        unit.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    }

    /// `DOORBELL_H264=recording.h264 cargo test -p comelit-hub-hap -- --ignored`
    /// with a recording of the real external unit (`viper-client start-video`).
    #[test]
    #[ignore]
    fn decodes_a_real_recording() {
        let path = std::env::var("DOORBELL_H264").expect("DOORBELL_H264 not set");
        let stream = std::fs::read(path).unwrap();
        // the first IDR frames are at the start of a recording
        let jpeg = decode_to_jpeg(&stream[..stream.len().min(200_000)]).unwrap();
        println!("snapshot {:?}, {} bytes", jpeg_size(&jpeg), jpeg.len());
        if let Ok(out) = std::env::var("DOORBELL_JPEG") {
            std::fs::write(out, &jpeg).unwrap();
        }
    }
}

//! Audio of the external unit for the HomeKit doorbell camera.
//!
//! The external unit sends G.711 A-law (PCMA) at 8 kHz, 20 ms per packet;
//! iOS only plays Opus or AAC-ELD, and the Home hubs relaying a stream ask
//! for AAC-ELD whatever is offered. The samples are decoded and encoded:
//!
//! - AAC-ELD with libfdk-aac at 16 kHz (the samples are upsampled), frames
//!   of 480 samples (30 ms), RTP as RFC 3640 with one access unit per 30 ms
//!   of the packet time HomeKit asks for (30 or 60 ms), clock of 16 kHz;
//! - Opus with libopus, which takes 8 kHz input as it is, frames of the
//!   packet time HomeKit asks for, RTP clock of 48 kHz (RFC 7587).
//!
//! Packets carry HomeKit's payload type and our audio SSRC, encrypted with
//! the audio SRTP keys, with an RTCP sender report every second as for the
//! video.

use std::sync::Arc;
use std::time::Duration;

use fdk_aac_sys as fdk;
use hap::accessory::camera::manager::SessionState;
use hap::accessory::camera::protocol::StreamAudio;
use opus_codec::types::{Application, Bitrate, Channels, SampleRate};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{debug, info};
use viper_client::video::media::RtpPacket;
use viper_client::video::{MediaKind, MediaPacket};
use webrtc_srtp::context::Context;
use webrtc_srtp::protection_profile::ProtectionProfile;

use crate::doorbell_camera::{CameraError, sender_report};

/// HAP audio codec types.
pub const CODEC_AAC_ELD: u8 = 2;
pub const CODEC_OPUS: u8 = 3;
/// Payload type of PCMA in the external unit RTP packets.
const PT_PCMA: u8 = 8;
const INPUT_RATE: u32 = 8000;
/// RTP clock of Opus, whatever the sample rate (RFC 7587).
const OPUS_RTP_CLOCK: u32 = 48_000;
/// Frame durations an Opus packet can have, in ms (one frame per packet).
const OPUS_FRAME_MS: [u8; 4] = [10, 20, 40, 60];
const DEFAULT_FRAME_MS: u8 = 20;
const MAX_PACKET: usize = 1500;
const AAC_RATE: u32 = 16_000;
/// AAC-ELD frame length at 16 kHz (30 ms).
const AAC_FRAME: usize = 480;
const AAC_FRAME_MS: u8 = 30;
/// Most access units in an RTP packet (120 ms).
const MAX_AAC_FRAMES: usize = 4;
const DEFAULT_AAC_KBPS: u16 = 24;
const RTCP_INTERVAL: Duration = Duration::from_secs(1);
/// Sender reports between two log lines of the audio statistics.
const STATS_REPORTS: u32 = 5;

/// Encodes frames of 8 kHz samples into RTP payloads.
trait FrameEncoder: Send {
    /// 8 kHz samples per frame.
    fn frame_samples(&self) -> usize;
    /// RTP timestamp increment per frame.
    fn timestamp_step(&self) -> u32;
    /// The RTP payload of a frame, if the encoder produced one.
    fn encode(&mut self, frame: &[i16]) -> Result<Option<Vec<u8>>, CameraError>;
}

struct Opus {
    encoder: opus_codec::Encoder,
    frame_samples: usize,
}

impl Opus {
    fn new(audio: &StreamAudio) -> Result<Opus, CameraError> {
        let frame_ms = if OPUS_FRAME_MS.contains(&audio.packet_time) {
            audio.packet_time
        } else {
            debug!(
                "Audio packet time of {} ms not possible with Opus, using {DEFAULT_FRAME_MS} ms",
                audio.packet_time
            );
            DEFAULT_FRAME_MS
        };
        let opus = |e: opus_codec::Error| CameraError::Audio(e.to_string());
        let mut encoder =
            opus_codec::Encoder::new(SampleRate::Hz8000, Channels::Mono, Application::Voip)
                .map_err(opus)?;
        if audio.max_bitrate > 0 {
            encoder
                .set_bitrate(Bitrate::Custom(i32::from(audio.max_bitrate) * 1000))
                .map_err(opus)?;
        }
        Ok(Opus {
            encoder,
            frame_samples: (INPUT_RATE / 1000) as usize * usize::from(frame_ms),
        })
    }
}

impl FrameEncoder for Opus {
    fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    fn timestamp_step(&self) -> u32 {
        self.frame_samples as u32 * (OPUS_RTP_CLOCK / INPUT_RATE)
    }

    fn encode(&mut self, frame: &[i16]) -> Result<Option<Vec<u8>>, CameraError> {
        let mut out = [0u8; MAX_PACKET];
        let len = self
            .encoder
            .encode(frame, &mut out)
            .map_err(|e| CameraError::Audio(e.to_string()))?;
        Ok(Some(out[..len].to_vec()))
    }
}

/// AAC-ELD encoder of libfdk-aac, mono 16 kHz, raw access units.
struct AacEld {
    handle: fdk::HANDLE_AACENCODER,
    /// Last input sample, for the upsampling.
    previous: i16,
    /// Access units per RTP packet, for the packet time HomeKit asks.
    frames: usize,
    /// Access units not sent yet.
    pending: Vec<Vec<u8>>,
}

// the handle is only used through `&mut self`
unsafe impl Send for AacEld {}

impl AacEld {
    fn new(audio: &StreamAudio) -> Result<AacEld, CameraError> {
        let check = |what: &str, result: fdk::AACENC_ERROR| {
            if result == fdk::AACENC_ERROR_AACENC_OK {
                Ok(())
            } else {
                Err(CameraError::Audio(format!(
                    "AAC-ELD {what}: error {result}"
                )))
            }
        };
        let kbps = if audio.max_bitrate > 0 {
            audio.max_bitrate
        } else {
            DEFAULT_AAC_KBPS
        };
        let mut handle: fdk::HANDLE_AACENCODER = std::ptr::null_mut();
        // SAFETY: plain libfdk-aac calls on a handle we own
        unsafe {
            check("open", fdk::aacEncOpen(&mut handle, 0, 1))?;
            let encoder = AacEld {
                handle,
                previous: 0,
                frames: usize::from(audio.packet_time / AAC_FRAME_MS).clamp(1, MAX_AAC_FRAMES),
                pending: Vec::new(),
            };
            for (what, param, value) in [
                (
                    "object type",
                    fdk::AACENC_PARAM_AACENC_AOT,
                    fdk::AUDIO_OBJECT_TYPE_AOT_ER_AAC_ELD as u32,
                ),
                ("sample rate", fdk::AACENC_PARAM_AACENC_SAMPLERATE, AAC_RATE),
                ("channels", fdk::AACENC_PARAM_AACENC_CHANNELMODE, 1),
                (
                    "bitrate",
                    fdk::AACENC_PARAM_AACENC_BITRATE,
                    u32::from(kbps) * 1000,
                ),
                (
                    "frame length",
                    fdk::AACENC_PARAM_AACENC_GRANULE_LENGTH,
                    AAC_FRAME as u32,
                ),
                ("transport", fdk::AACENC_PARAM_AACENC_TRANSMUX, 0),
            ] {
                check(what, fdk::aacEncoder_SetParam(handle, param, value))?;
            }
            check(
                "init",
                fdk::aacEncEncode(
                    handle,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                ),
            )?;
            Ok(encoder)
        }
    }

    /// 8 kHz to 16 kHz by linear interpolation.
    fn upsample(&mut self, frame: &[i16]) -> Vec<i16> {
        let mut out = Vec::with_capacity(frame.len() * 2);
        for &sample in frame {
            out.push(((i32::from(self.previous) + i32::from(sample)) / 2) as i16);
            out.push(sample);
            self.previous = sample;
        }
        out
    }
}

impl Drop for AacEld {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by aacEncOpen and is closed once
        unsafe {
            fdk::aacEncClose(&mut self.handle);
        }
    }
}

impl FrameEncoder for AacEld {
    fn frame_samples(&self) -> usize {
        AAC_FRAME / 2 * self.frames
    }

    fn timestamp_step(&self) -> u32 {
        (AAC_FRAME * self.frames) as u32
    }

    fn encode(&mut self, frame: &[i16]) -> Result<Option<Vec<u8>>, CameraError> {
        let input = self.upsample(frame);
        for granule in input.chunks(AAC_FRAME) {
            if let Some(access_unit) = self.encode_granule(granule)? {
                self.pending.push(access_unit);
            }
        }
        if self.pending.len() < self.frames {
            // the encoder delay
            return Ok(None);
        }
        let access_units: Vec<Vec<u8>> = self.pending.drain(..self.frames).collect();
        Ok(Some(au_packet(&access_units)))
    }
}

impl AacEld {
    /// One access unit of 480 samples at 16 kHz, if the encoder produced
    /// one.
    fn encode_granule(&mut self, input: &[i16]) -> Result<Option<Vec<u8>>, CameraError> {
        use std::ffi::{c_int, c_void};
        let mut output = [0u8; MAX_PACKET];
        let mut in_buf = input.as_ptr() as *mut c_void;
        let mut in_id = fdk::AACENC_BufferIdentifier_IN_AUDIO_DATA as c_int;
        let mut in_size = (input.len() * 2) as c_int;
        let mut in_el_size = 2 as c_int;
        let mut out_buf = output.as_mut_ptr() as *mut c_void;
        let mut out_id = fdk::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as c_int;
        let mut out_size = output.len() as c_int;
        let mut out_el_size = 1 as c_int;
        let in_desc = fdk::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut in_buf,
            bufferIdentifiers: &mut in_id,
            bufSizes: &mut in_size,
            bufElSizes: &mut in_el_size,
        };
        let out_desc = fdk::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut out_buf,
            bufferIdentifiers: &mut out_id,
            bufSizes: &mut out_size,
            bufElSizes: &mut out_el_size,
        };
        let in_args = fdk::AACENC_InArgs {
            numInSamples: input.len() as c_int,
            numAncBytes: 0,
        };
        // SAFETY: the descriptors point to buffers alive for the call
        let (result, len) = unsafe {
            let mut out_args: fdk::AACENC_OutArgs = std::mem::zeroed();
            let result =
                fdk::aacEncEncode(self.handle, &in_desc, &out_desc, &in_args, &mut out_args);
            (result, out_args.numOutBytes as usize)
        };
        if result != fdk::AACENC_ERROR_AACENC_OK {
            return Err(CameraError::Audio(format!(
                "AAC-ELD encode: error {result}"
            )));
        }
        if len == 0 {
            return Ok(None);
        }
        Ok(Some(output[..len].to_vec()))
    }
}

/// RFC 3640 (mpeg4-generic, AAC-hbr) payload of consecutive access units:
/// the AU headers length in bits (16), one AU header per access unit (a 13
/// bit size and a 3 bit index or index delta, always 0), then the access
/// units.
fn au_packet(access_units: &[Vec<u8>]) -> Vec<u8> {
    let headers_bits = (access_units.len() * 16) as u16;
    let mut payload = headers_bits.to_be_bytes().to_vec();
    for access_unit in access_units {
        payload.extend_from_slice(&((access_unit.len() as u16) << 3).to_be_bytes());
    }
    for access_unit in access_units {
        payload.extend_from_slice(access_unit);
    }
    payload
}

/// Encodes the external unit audio for one HomeKit session and encrypts it.
pub struct AudioSender {
    srtp: Context,
    encoder: Box<dyn FrameEncoder>,
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    pcm: Vec<i16>,
    packets: u32,
    octets: u32,
}

impl AudioSender {
    /// An Opus or AAC-ELD sender, as HomeKit chose.
    pub fn new(session: &SessionState, audio: &StreamAudio) -> Result<AudioSender, CameraError> {
        let encoder: Box<dyn FrameEncoder> = match audio.codec.first() {
            Some(&CODEC_OPUS) => Box::new(Opus::new(audio)?),
            Some(&CODEC_AAC_ELD) => Box::new(AacEld::new(audio)?),
            _ => {
                return Err(CameraError::Audio(format!(
                    "codec {:?} not supported",
                    audio.codec
                )));
            }
        };
        let crypto = &session.audio_crypto;
        let srtp = Context::new(
            &crypto.master_key,
            &crypto.master_salt,
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .map_err(|e| CameraError::Srtp(e.to_string()))?;
        Ok(AudioSender {
            srtp,
            encoder,
            payload_type: audio.payload_type.first().copied().unwrap_or(110),
            ssrc: session.audio_ssrc,
            sequence: rand::random(),
            timestamp: rand::random(),
            pcm: Vec::new(),
            packets: 0,
            octets: 0,
        })
    }

    /// Takes an RTP packet of the external unit: the encrypted packets of
    /// every frame completed by it.
    pub fn push(&mut self, rtp: &[u8]) -> Result<Vec<Vec<u8>>, CameraError> {
        let Some(packet) = RtpPacket::parse(rtp) else {
            return Ok(vec![]);
        };
        if packet.payload_type != PT_PCMA {
            return Ok(vec![]);
        }
        self.pcm
            .extend(packet.payload.iter().map(|&b| alaw_to_linear(b)));
        let frame_samples = self.encoder.frame_samples();
        let mut out = vec![];
        while self.pcm.len() >= frame_samples {
            let frame: Vec<i16> = self.pcm.drain(..frame_samples).collect();
            let step = self.encoder.timestamp_step();
            let Some(payload) = self.encoder.encode(&frame)? else {
                // the encoder delay: the timeline goes on
                self.timestamp = self.timestamp.wrapping_add(step);
                continue;
            };
            let rtp = self.packet(&payload, step);
            out.push(
                self.srtp
                    .encrypt_rtp(&rtp)
                    .map_err(|e| CameraError::Srtp(e.to_string()))?
                    .to_vec(),
            );
        }
        Ok(out)
    }

    fn packet(&mut self, payload: &[u8], step: u32) -> Vec<u8> {
        let mut rtp = Vec::with_capacity(12 + payload.len());
        rtp.extend_from_slice(&[0x80, 0x80 | self.payload_type]);
        rtp.extend_from_slice(&self.sequence.to_be_bytes());
        rtp.extend_from_slice(&self.timestamp.to_be_bytes());
        rtp.extend_from_slice(&self.ssrc.to_be_bytes());
        rtp.extend_from_slice(payload);
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(step);
        self.packets = self.packets.wrapping_add(1);
        self.octets = self.octets.wrapping_add(payload.len() as u32);
        rtp
    }

    fn report(&mut self) -> Result<Vec<u8>, CameraError> {
        let sr = sender_report(self.ssrc, self.timestamp, self.packets, self.octets);
        Ok(self
            .srtp
            .encrypt_rtcp(&sr)
            .map_err(|e| CameraError::Srtp(e.to_string()))?
            .to_vec())
    }

    /// Sends the audio of `packets` until the feed ends or the task is
    /// aborted.
    pub async fn forward(
        &mut self,
        mut packets: broadcast::Receiver<Arc<MediaPacket>>,
        socket: UdpSocket,
    ) -> Result<(), CameraError> {
        let mut report = tokio::time::interval(RTCP_INTERVAL);
        let mut reports = 0u32;
        let mut buf = vec![0u8; 2048];
        loop {
            tokio::select! {
                packet = packets.recv() => match packet {
                    Ok(packet) if packet.kind == MediaKind::Audio => {
                        for encrypted in self.push(&packet.rtp)? {
                            if let Err(e) = socket.send(&encrypted).await {
                                debug!("Cannot send audio RTP: {e}");
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!("Doorbell audio: {n} packets dropped");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("The doorbell audio ended");
                        return Ok(());
                    }
                },
                // the RTCP of the HomeKit device: nothing to do with it yet
                _ = socket.recv(&mut buf) => {}
                _ = report.tick() => {
                    reports += 1;
                    if reports.is_multiple_of(STATS_REPORTS) {
                        info!(
                            "Doorbell audio: {} packets, {} bytes sent in total",
                            self.packets, self.octets
                        );
                    }
                    if self.packets == 0 {
                        continue;
                    }
                    if let Err(e) = socket.send(&self.report()?).await {
                        debug!("Cannot send audio RTCP: {e}");
                    }
                }
            }
        }
    }
}

/// G.711 A-law sample to 16 bit linear PCM.
fn alaw_to_linear(byte: u8) -> i16 {
    let a = byte ^ 0x55;
    let segment = (a & 0x70) >> 4;
    let mut value = i16::from(a & 0x0f) << 4;
    value = match segment {
        0 => value + 8,
        1 => value + 0x108,
        _ => (value + 0x108) << (segment - 1),
    };
    if a & 0x80 != 0 { value } else { -value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hap::accessory::camera::protocol::SetupEndpointCrypto;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;

    fn session() -> SessionState {
        let crypto = SetupEndpointCrypto {
            cypto_suite: 0,
            master_key: vec![3; 16],
            master_salt: vec![5; 14],
        };
        SessionState {
            session_id: Uuid::nil(),
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            video_port: 0,
            audio_port: 0,
            video_crypto: crypto.clone(),
            audio_crypto: crypto,
            video_ssrc: 1,
            audio_ssrc: 0xa0d1_0000,
        }
    }

    fn stream_audio(packet_time: u8) -> StreamAudio {
        StreamAudio {
            codec: vec![CODEC_OPUS],
            comfort_noise: vec![0],
            channel: 1,
            bitrate: false,
            sample_rate: 24,
            packet_time,
            ssrc: 42,
            payload_type: vec![110],
            max_bitrate: 24,
            rtcp_interval: 5.0,
            comfort_payload_type: vec![13],
        }
    }

    /// A PCMA packet of the external unit: 20 ms of a 440 Hz tone.
    fn pcma_packet(seq: u16) -> Vec<u8> {
        let mut rtp = vec![0x80, PT_PCMA];
        rtp.extend_from_slice(&seq.to_be_bytes());
        rtp.extend_from_slice(&(u32::from(seq) * 160).to_be_bytes());
        rtp.extend_from_slice(&7u32.to_be_bytes());
        rtp.extend((0..160).map(|n| {
            let t = (u32::from(seq) * 160 + n) as f32 / 8000.0;
            linear_to_alaw((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0)
        }));
        rtp
    }

    fn linear_to_alaw(sample: f32) -> u8 {
        // the encoder of the decoded value closest to the sample
        (0..=255u8)
            .min_by_key(|&b| (i32::from(alaw_to_linear(b)) - sample as i32).abs())
            .unwrap()
    }

    fn decrypt(session: &SessionState, packets: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let crypto = &session.audio_crypto;
        let mut ctx = Context::new(
            &crypto.master_key,
            &crypto.master_salt,
            ProtectionProfile::Aes128CmHmacSha1_80,
            None,
            None,
        )
        .unwrap();
        packets
            .iter()
            .map(|p| ctx.decrypt_rtp(p).unwrap().to_vec())
            .collect()
    }

    #[test]
    fn decodes_alaw() {
        assert_eq!(alaw_to_linear(0xd5), 8);
        assert_eq!(alaw_to_linear(0x55), -8);
        assert_eq!(alaw_to_linear(0xaa), 32256);
        assert_eq!(alaw_to_linear(0x2a), -32256);
    }

    #[test]
    fn encodes_opus_frames_of_the_asked_packet_time() {
        let session = session();
        let mut sender = AudioSender::new(&session, &stream_audio(40)).unwrap();
        // 40 ms frames: one packet every two PCMA packets
        let mut sent = vec![];
        for seq in 0..6 {
            sent.extend(sender.push(&pcma_packet(seq)).unwrap());
        }
        assert_eq!(sent.len(), 3);

        let rtp = decrypt(&session, &sent);
        let first = RtpPacket::parse(&rtp[0]).unwrap();
        let second = RtpPacket::parse(&rtp[1]).unwrap();
        assert_eq!(first.payload_type, 110);
        assert_eq!(first.ssrc, 0xa0d1_0000);
        assert_eq!(second.sequence, first.sequence.wrapping_add(1));
        // 40 ms at the 48 kHz Opus clock
        assert_eq!(second.timestamp.wrapping_sub(first.timestamp), 1920);

        // the payloads are Opus packets of 40 ms
        let mut decoder = opus_codec::Decoder::new(SampleRate::Hz8000, Channels::Mono).unwrap();
        let mut pcm = [0i16; 960];
        let samples = decoder.decode(second.payload, &mut pcm, false).unwrap();
        assert_eq!(samples, 320);
    }

    #[test]
    fn encodes_aac_eld_access_units() {
        let session = session();
        let audio = StreamAudio {
            codec: vec![CODEC_AAC_ELD],
            packet_time: 30,
            sample_rate: 16,
            ..stream_audio(30)
        };
        let mut sender = AudioSender::new(&session, &audio).unwrap();
        let mut sent = vec![];
        for seq in 0..30 {
            sent.extend(sender.push(&pcma_packet(seq)).unwrap());
        }
        // 600 ms of audio in 30 ms frames, less the encoder delay
        assert!((15..=20).contains(&sent.len()), "{} packets", sent.len());

        let rtp = decrypt(&session, &sent);
        let first = RtpPacket::parse(&rtp[0]).unwrap();
        let second = RtpPacket::parse(&rtp[1]).unwrap();
        assert_eq!(first.payload_type, 110);
        // 480 samples at the 16 kHz clock
        assert_eq!(second.timestamp.wrapping_sub(first.timestamp), 480);
        // one AU header, with the size of the access unit after it
        let payload = second.payload;
        assert_eq!(&payload[..2], &[0x00, 0x10]);
        let size = u16::from_be_bytes([payload[2], payload[3]]) >> 3;
        assert_eq!(usize::from(size), payload.len() - 4);
    }

    #[test]
    fn packs_two_aac_eld_access_units_in_60_ms() {
        let session = session();
        let audio = StreamAudio {
            codec: vec![CODEC_AAC_ELD],
            packet_time: 60,
            sample_rate: 16,
            ..stream_audio(60)
        };
        let mut sender = AudioSender::new(&session, &audio).unwrap();
        let mut sent = vec![];
        for seq in 0..30 {
            sent.extend(sender.push(&pcma_packet(seq)).unwrap());
        }
        // 600 ms of audio in 60 ms packets, less the encoder delay
        assert!((7..=10).contains(&sent.len()), "{} packets", sent.len());

        let rtp = decrypt(&session, &sent);
        let first = RtpPacket::parse(&rtp[0]).unwrap();
        let second = RtpPacket::parse(&rtp[1]).unwrap();
        // 960 samples at the 16 kHz clock
        assert_eq!(second.timestamp.wrapping_sub(first.timestamp), 960);
        // two AU headers, then the two access units
        let payload = second.payload;
        assert_eq!(&payload[..2], &[0x00, 0x20]);
        let first_size = usize::from(u16::from_be_bytes([payload[2], payload[3]]) >> 3);
        let second_size = usize::from(u16::from_be_bytes([payload[4], payload[5]]) >> 3);
        assert!(first_size > 0 && second_size > 0);
        assert_eq!(first_size + second_size, payload.len() - 6);
    }

    #[test]
    fn falls_back_to_20_ms_frames() {
        let mut sender = AudioSender::new(&session(), &stream_audio(30)).unwrap();
        assert_eq!(sender.push(&pcma_packet(0)).unwrap().len(), 1);
    }

    #[test]
    fn ignores_other_payloads() {
        let mut sender = AudioSender::new(&session(), &stream_audio(20)).unwrap();
        let mut video = pcma_packet(0);
        video[1] = 99;
        assert!(sender.push(&video).unwrap().is_empty());
    }
}

//! Our voice to the external unit: the audio the HomeKit device sends
//! while the Home app microphone is on.
//!
//! The packets arrive on the audio socket of the stream (rtcp-mux), with
//! the audio SRTP keys (the same both ways) and the codec HomeKit chose:
//!
//! - AAC-ELD as RFC 3640, decoded with libfdk-aac (configured like our
//!   encoder) and brought from its sample rate down to 8 kHz;
//! - Opus, decoded by libopus straight at 8 kHz.
//!
//! The samples go to the external unit as PCMA in 20 ms packets
//! ([`ViperUnit::send_voice`]). The reception of the HomeKit stream is
//! reported in our audio sender reports.

use fdk_aac_sys as fdk;
use hap::accessory::camera::manager::SessionState;
use hap::accessory::camera::protocol::StreamAudio;
use opus_codec::types::{Channels, SampleRate};
use tracing::{debug, info};
use viper_client::video::ViperUnit;
use viper_client::video::media::RtpPacket;
use webrtc_srtp::context::Context;
use webrtc_srtp::protection_profile::ProtectionProfile;

use crate::doorbell_audio::{CODEC_AAC_ELD, CODEC_OPUS, aac_eld_config};
use crate::doorbell_camera::{CameraError, RTCP_PSFB, RTCP_SR};

const OUTPUT_RATE: usize = 8000;
/// Samples of a PCMA packet to the external unit (20 ms).
const VOICE_PACKET: usize = 160;
/// Longest Opus packet (120 ms) at 8 kHz.
const OPUS_MAX_SAMPLES: usize = 960;
/// Room for the samples of one AAC frame.
const AAC_MAX_SAMPLES: usize = 2048;

/// Decodes the RTP payloads of the HomeKit device into 8 kHz samples.
trait FrameDecoder: Send {
    fn decode(&mut self, payload: &[u8]) -> Result<Vec<i16>, CameraError>;
}

struct Opus(opus_codec::Decoder);

impl FrameDecoder for Opus {
    fn decode(&mut self, payload: &[u8]) -> Result<Vec<i16>, CameraError> {
        let mut out = [0i16; OPUS_MAX_SAMPLES];
        let samples = self
            .0
            .decode(payload, &mut out, false)
            .map_err(|e| CameraError::Audio(e.to_string()))?;
        Ok(out[..samples].to_vec())
    }
}

/// AAC-ELD decoder of libfdk-aac, raw access units.
struct AacEld {
    handle: fdk::HANDLE_AACDECODER,
}

// the handle is only used through `&mut self`
unsafe impl Send for AacEld {}

impl AacEld {
    fn new(audio: &StreamAudio) -> Result<AacEld, CameraError> {
        let mut config = aac_eld_config(audio)?;
        // SAFETY: plain libfdk-aac calls on a handle we own
        unsafe {
            let handle = fdk::aacDecoder_Open(fdk::TRANSPORT_TYPE_TT_MP4_RAW, 1);
            if handle.is_null() {
                return Err(CameraError::Audio("AAC-ELD decoder open failed".into()));
            }
            let decoder = AacEld { handle };
            let mut buffer = config.as_mut_ptr();
            let length = config.len() as u32;
            let result = fdk::aacDecoder_ConfigRaw(handle, &mut buffer, &length);
            if result != fdk::AAC_DECODER_ERROR_AAC_DEC_OK {
                return Err(CameraError::Audio(format!(
                    "AAC-ELD decoder config: error {result}"
                )));
            }
            Ok(decoder)
        }
    }

    /// The samples of one access unit, at the stream sample rate.
    fn decode_access_unit(&mut self, access_unit: &[u8]) -> Result<(Vec<i16>, usize), CameraError> {
        let mut data = access_unit.to_vec();
        let mut out = vec![0i16; AAC_MAX_SAMPLES];
        // SAFETY: the buffers outlive the calls, sizes are passed along
        unsafe {
            let mut buffer = data.as_mut_ptr();
            let size = data.len() as u32;
            let mut valid = size;
            let result = fdk::aacDecoder_Fill(self.handle, &mut buffer, &size, &mut valid);
            if result != fdk::AAC_DECODER_ERROR_AAC_DEC_OK {
                return Err(CameraError::Audio(format!("AAC-ELD fill: error {result}")));
            }
            let result =
                fdk::aacDecoder_DecodeFrame(self.handle, out.as_mut_ptr(), out.len() as i32, 0);
            if result == fdk::AAC_DECODER_ERROR_AAC_DEC_NOT_ENOUGH_BITS {
                return Ok((vec![], 0));
            }
            if result != fdk::AAC_DECODER_ERROR_AAC_DEC_OK {
                return Err(CameraError::Audio(format!(
                    "AAC-ELD decode: error {result}"
                )));
            }
            let info = &*fdk::aacDecoder_GetStreamInfo(self.handle);
            let samples = (info.frameSize * info.numChannels).max(0) as usize;
            out.truncate(samples.min(AAC_MAX_SAMPLES));
            Ok((out, info.sampleRate.max(0) as usize))
        }
    }
}

impl Drop for AacEld {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is not used afterwards
        unsafe { fdk::aacDecoder_Close(self.handle) }
    }
}

impl FrameDecoder for AacEld {
    fn decode(&mut self, payload: &[u8]) -> Result<Vec<i16>, CameraError> {
        let mut out = vec![];
        for access_unit in access_units(payload) {
            let (samples, rate) = self.decode_access_unit(access_unit)?;
            out.extend(downsample(&samples, rate));
        }
        Ok(out)
    }
}

/// The access units of an RFC 3640 (AAC-hbr) payload: 16 bit AU headers,
/// a 13 bit size each.
fn access_units(payload: &[u8]) -> Vec<&[u8]> {
    let Some(header_bits) = payload.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]])) else {
        return vec![];
    };
    let headers = usize::from(header_bits / 16);
    let mut offset = 2 + usize::from(header_bits).div_ceil(8);
    let mut units = Vec::with_capacity(headers);
    for header in payload.get(2..2 + headers * 2).unwrap_or(&[]).chunks(2) {
        let size = usize::from(u16::from_be_bytes([header[0], header[1]]) >> 3);
        let Some(unit) = payload.get(offset..offset + size) else {
            break;
        };
        units.push(unit);
        offset += size;
    }
    units
}

/// Averages the samples down to 8 kHz (the average is the low pass).
fn downsample(samples: &[i16], rate: usize) -> Vec<i16> {
    let factor = (rate / OUTPUT_RATE).max(1);
    samples
        .chunks(factor)
        .map(|c| (c.iter().map(|&s| i32::from(s)).sum::<i32>() / c.len() as i32) as i16)
        .collect()
}

/// 16 bit linear PCM to G.711 A-law (`linear2alaw` of the reference code).
fn linear_to_alaw(sample: i16) -> u8 {
    const SEGMENT_ENDS: [i32; 8] = [0x1f, 0x3f, 0x7f, 0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff];
    let mut value = i32::from(sample) >> 3;
    let mask = if value >= 0 {
        0xd5
    } else {
        value = -value - 1;
        0x55
    };
    let Some(segment) = SEGMENT_ENDS.iter().position(|&end| value <= end) else {
        return 0x7f ^ mask;
    };
    let quantized = if segment < 2 {
        value >> 1
    } else {
        value >> segment
    };
    (((segment as i32) << 4 | (quantized & 0x0f)) as u8) ^ mask
}

/// What we received of the HomeKit stream, for the report block of our
/// sender reports (RFC 3550 §6.4.1, A.3).
#[derive(Default)]
struct Reception {
    ssrc: u32,
    base_sequence: u32,
    /// Highest sequence, extended with the wrap arounds.
    highest: u32,
    received: u32,
    expected_prior: u32,
    received_prior: u32,
}

impl Reception {
    fn update(&mut self, ssrc: u32, sequence: u16) {
        if self.received == 0 || ssrc != self.ssrc {
            *self = Reception {
                ssrc,
                base_sequence: u32::from(sequence),
                highest: u32::from(sequence),
                ..Reception::default()
            };
        } else {
            let cycles = self.highest & 0xffff_0000;
            let last = self.highest as u16;
            let delta = sequence.wrapping_sub(last);
            if delta != 0 && delta < 0x8000 {
                let wrapped = if sequence < last { 0x1_0000 } else { 0 };
                self.highest = (cycles + wrapped) | u32::from(sequence);
            }
        }
        self.received += 1;
    }

    fn block(&mut self) -> [u8; 24] {
        let expected = self.highest.wrapping_sub(self.base_sequence) + 1;
        let lost = i64::from(expected) - i64::from(self.received);
        let expected_interval = expected.wrapping_sub(self.expected_prior);
        let received_interval = self.received.wrapping_sub(self.received_prior);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let lost_interval = i64::from(expected_interval) - i64::from(received_interval);
        let fraction = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << 8) / i64::from(expected_interval)) as u8
        };
        let cumulative = lost.clamp(-0x80_0000, 0x7f_ffff) as i32;
        let mut block = [0u8; 24];
        block[..4].copy_from_slice(&self.ssrc.to_be_bytes());
        block[4] = fraction;
        block[5..8].copy_from_slice(&cumulative.to_be_bytes()[1..]);
        block[8..12].copy_from_slice(&self.highest.to_be_bytes());
        // jitter, last SR and its delay unknown
        block
    }
}

/// Receives the audio of the HomeKit device and sends it to the external
/// unit.
pub struct VoiceReceiver {
    srtp: Context,
    decoder: Box<dyn FrameDecoder>,
    payload_type: Option<u8>,
    unit: Option<ViperUnit>,
    /// 8 kHz samples not sent yet.
    pcm: Vec<i16>,
    reception: Reception,
    /// PCMA packets handed to the unit.
    sent: u32,
}

impl VoiceReceiver {
    /// A receiver for the codec HomeKit chose; our voice is dropped when
    /// there is no unit.
    pub fn new(
        session: &SessionState,
        audio: &StreamAudio,
        unit: Option<ViperUnit>,
    ) -> Result<VoiceReceiver, CameraError> {
        let decoder: Box<dyn FrameDecoder> = match audio.codec.first() {
            Some(&CODEC_OPUS) => Box::new(Opus(
                opus_codec::Decoder::new(SampleRate::Hz8000, Channels::Mono)
                    .map_err(|e| CameraError::Audio(e.to_string()))?,
            )),
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
        Ok(VoiceReceiver {
            srtp,
            decoder,
            payload_type: audio.payload_type.first().copied(),
            unit,
            pcm: Vec::new(),
            reception: Reception::default(),
            sent: 0,
        })
    }

    /// Takes a packet of the audio socket: true when it was our voice.
    /// RTCP and packets that do not decrypt are left alone.
    pub fn receive(&mut self, packet: &[u8]) -> bool {
        // rtcp-mux: RTCP has the payload types 200-206 (RFC 5761)
        if packet.len() < 12 || packet[0] >> 6 != 2 || (RTCP_SR..=RTCP_PSFB).contains(&packet[1]) {
            return false;
        }
        let plain = match self.srtp.decrypt_rtp(packet) {
            Ok(plain) => plain,
            Err(e) => {
                debug!("Cannot decrypt the HomeKit audio: {e}");
                return false;
            }
        };
        let Some(rtp) = RtpPacket::parse(&plain) else {
            return false;
        };
        if self.payload_type.is_some_and(|pt| pt != rtp.payload_type) {
            debug!(
                "HomeKit audio with payload type {} ignored",
                rtp.payload_type
            );
            return false;
        }
        let ssrc = u32::from_be_bytes([plain[8], plain[9], plain[10], plain[11]]);
        let sequence = u16::from_be_bytes([plain[2], plain[3]]);
        if self.reception.received == 0 {
            info!("HomeKit sends our voice");
        }
        self.reception.update(ssrc, sequence);
        match self.decoder.decode(rtp.payload) {
            Ok(samples) => self.pcm.extend(samples),
            Err(e) => debug!("Cannot decode the HomeKit audio: {e}"),
        }
        while self.pcm.len() >= VOICE_PACKET {
            let pcma: Vec<u8> = self.pcm.drain(..VOICE_PACKET).map(linear_to_alaw).collect();
            if let Some(unit) = &self.unit {
                unit.send_voice(pcma);
            }
            self.sent += 1;
        }
        true
    }

    /// The report block of the HomeKit stream, once something arrived.
    pub fn report_block(&mut self) -> Option<[u8; 24]> {
        (self.reception.received > 0).then(|| self.reception.block())
    }
}

/// Adds a reception report block to a sender report without one.
pub(crate) fn with_report_block(mut sr: Vec<u8>, block: &[u8; 24]) -> Vec<u8> {
    sr[0] |= 1;
    let words = (sr.len() + block.len()) / 4 - 1;
    sr[2..4].copy_from_slice(&(words as u16).to_be_bytes());
    sr.extend_from_slice(block);
    sr
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doorbell_audio::tests as audio_tests;
    use crate::doorbell_audio::{AudioSender, alaw_to_linear};
    use crate::doorbell_camera::sender_report;

    #[test]
    fn encodes_alaw_like_the_reference() {
        for sample in [-32768i16, -1000, -1, 0, 1, 7, 500, 4000, 32767] {
            let back = i32::from(alaw_to_linear(linear_to_alaw(sample)));
            // the quantization step grows with the level
            let tolerance = (i32::from(sample).abs() / 16).max(16);
            assert!(
                (back - i32::from(sample)).abs() <= tolerance,
                "{sample} -> {back}"
            );
        }
        assert_eq!(linear_to_alaw(0), 0xd5);
        assert_eq!(linear_to_alaw(-1), 0x55);
    }

    #[test]
    fn splits_aac_hbr_access_units() {
        // two AU headers: sizes 3 and 2
        let payload = [0x00, 0x20, 0x00, 0x18, 0x00, 0x10, 1, 2, 3, 4, 5];
        assert_eq!(access_units(&payload), vec![&[1u8, 2, 3][..], &[4, 5][..]]);
        assert!(access_units(&[0x00]).is_empty());
        // a size past the end
        assert_eq!(access_units(&[0x00, 0x10, 0x00, 0x50, 1]).len(), 0);
    }

    #[test]
    fn downsamples_to_8_khz() {
        assert_eq!(downsample(&[10, 20, 30, 50], 16_000), vec![15, 40]);
        assert_eq!(downsample(&[1, 2, 3], 8000), vec![1, 2, 3]);
    }

    #[test]
    fn reports_losses_and_wrap_arounds() {
        let mut reception = Reception::default();
        for sequence in [0xfffe, 0xffff, 1, 2] {
            reception.update(7, sequence);
        }
        let block = reception.block();
        assert_eq!(&block[..4], &7u32.to_be_bytes());
        // 5 expected, 4 received
        assert_eq!(block[4], (256 / 5) as u8);
        assert_eq!(&block[5..8], &[0, 0, 1]);
        assert_eq!(&block[8..12], &0x0001_0002u32.to_be_bytes());
        // nothing new: no loss in the interval
        assert_eq!(reception.block()[4], 0);
    }

    #[test]
    fn appends_the_report_block() {
        let sr = with_report_block(sender_report(1, 2, 3, 4), &[9; 24]);
        assert_eq!(sr.len(), 52);
        assert_eq!(sr[0], 0x81);
        assert_eq!(u16::from_be_bytes([sr[2], sr[3]]), 12);
        assert_eq!(&sr[28..], &[9; 24]);
    }

    /// Our own audio, encrypted by our sender, goes through the receiver:
    /// the PCMA packets it would hand to the unit.
    fn round_trip(codec: u8, packet_time: u8) -> (u32, u32) {
        let session = audio_tests::session();
        let audio = StreamAudio {
            codec: vec![codec],
            sample_rate: 16,
            ..audio_tests::stream_audio(packet_time)
        };
        let mut sender = AudioSender::new(&session, &audio).unwrap();
        let mut receiver = VoiceReceiver::new(&session, &audio, None).unwrap();
        // a second of the external unit audio
        for seq in 0..50 {
            for packet in sender.push(&audio_tests::pcma_packet(seq)).unwrap() {
                assert!(receiver.receive(&packet));
            }
        }
        // RTCP is not our voice
        assert!(!receiver.receive(&sender_report(1, 2, 3, 4)));
        (receiver.reception.received, receiver.sent)
    }

    #[test]
    fn decodes_aac_eld_voice() {
        let (received, sent) = round_trip(CODEC_AAC_ELD, 60);
        assert!(received >= 15, "{received} packets");
        // a second of 20 ms packets, less the codec delay
        assert!((40..=50).contains(&sent), "{sent} voice packets");
    }

    #[test]
    fn decodes_opus_voice() {
        let (received, sent) = round_trip(CODEC_OPUS, 20);
        assert_eq!(received, 50);
        assert_eq!(sent, 50);
    }
}

//! RTP handling for the media received on RTPC channels.
//!
//! Payload types used by the Viper engine (`Rtp::rtp_rx_new_session`):
//! 99 = H.264, 104 = MPEG-4 part 2, 26 = MJPEG, 8 = PCMA, 0 = PCMU.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use tracing::{debug, info, warn};

pub const PT_PCMU: u8 = 0;
pub const PT_PCMA: u8 = 8;
pub const PT_MJPEG: u8 = 26;
pub const PT_H264: u8 = 99;
pub const PT_MPEG4: u8 = 104;

const ANNEX_B_START_CODE: [u8; 4] = [0, 0, 0, 1];
const NAL_STAP_A: u8 = 24;
const NAL_FU_A: u8 = 28;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    pub payload_type: u8,
    pub marker: bool,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: &'a [u8],
}

impl<'a> RtpPacket<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<RtpPacket<'a>> {
        if buf.len() < 12 || buf[0] >> 6 != 2 {
            return None;
        }
        let csrc_count = (buf[0] & 0x0f) as usize;
        let mut start = 12 + csrc_count * 4;
        if buf[0] & 0x10 != 0 {
            // Header extension: 16 bit profile, 16 bit length in 32 bit words
            if buf.len() < start + 4 {
                return None;
            }
            let words = u16::from_be_bytes([buf[start + 2], buf[start + 3]]) as usize;
            start += 4 + words * 4;
        }
        let mut end = buf.len();
        if buf[0] & 0x20 != 0 {
            end = end.checked_sub(*buf.last()? as usize)?;
        }
        if start > end {
            return None;
        }
        Some(RtpPacket {
            payload_type: buf[1] & 0x7f,
            marker: buf[1] & 0x80 != 0,
            sequence: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
            payload: &buf[start..end],
        })
    }
}

/// Rebuilds H.264 NAL units from RTP payloads (RFC 6184: single NAL unit,
/// STAP-A and FU-A packets).
#[derive(Default)]
pub struct H264Depacketizer {
    fragment: Option<Vec<u8>>,
}

impl H264Depacketizer {
    /// Signals packet loss: a partially received fragmented NAL is discarded.
    pub fn reset(&mut self) {
        self.fragment = None;
    }

    pub fn push(&mut self, payload: &[u8]) -> Vec<Vec<u8>> {
        let Some(&indicator) = payload.first() else {
            return vec![];
        };
        match indicator & 0x1f {
            NAL_STAP_A => {
                let mut nals = vec![];
                let mut rest = &payload[1..];
                while rest.len() >= 2 {
                    let size = u16::from_be_bytes([rest[0], rest[1]]) as usize;
                    if rest.len() < 2 + size {
                        break;
                    }
                    nals.push(rest[2..2 + size].to_vec());
                    rest = &rest[2 + size..];
                }
                nals
            }
            NAL_FU_A => {
                if payload.len() < 2 {
                    return vec![];
                }
                let header = payload[1];
                if header & 0x80 != 0 {
                    let mut nal = vec![(indicator & 0xe0) | (header & 0x1f)];
                    nal.extend_from_slice(&payload[2..]);
                    self.fragment = Some(nal);
                } else if let Some(nal) = self.fragment.as_mut() {
                    nal.extend_from_slice(&payload[2..]);
                }
                if header & 0x40 != 0 {
                    return self.fragment.take().into_iter().collect();
                }
                vec![]
            }
            _ => vec![payload.to_vec()],
        }
    }
}

/// Writes one media stream to disk, choosing the format from the first RTP
/// payload type it sees: `<prefix>.h264` (Annex B), `<prefix>.m4v`,
/// `<prefix>.alaw` or `<prefix>.ulaw`.
pub struct StreamRecorder {
    prefix: PathBuf,
    out: Option<(u8, BufWriter<File>)>,
    depacketizer: H264Depacketizer,
    last_sequence: Option<u16>,
    pub packets: usize,
    pub lost: usize,
    pub units: usize,
}

impl StreamRecorder {
    pub fn new(prefix: impl Into<PathBuf>) -> StreamRecorder {
        StreamRecorder {
            prefix: prefix.into(),
            out: None,
            depacketizer: H264Depacketizer::default(),
            last_sequence: None,
            packets: 0,
            lost: 0,
            units: 0,
        }
    }

    pub fn output(&self) -> Option<PathBuf> {
        self.out
            .as_ref()
            .and_then(|(pt, _)| extension(*pt))
            .map(|ext| self.prefix.with_extension(ext))
    }

    pub fn push(&mut self, packet: &RtpPacket) -> io::Result<()> {
        self.packets += 1;
        if let Some(last) = self.last_sequence {
            let gap = packet.sequence.wrapping_sub(last);
            if gap == 0 || gap > 0x8000 {
                debug!("Dropping duplicate/late RTP packet {}", packet.sequence);
                return Ok(());
            }
            if gap > 1 {
                self.lost += (gap - 1) as usize;
                self.depacketizer.reset();
            }
        }
        self.last_sequence = Some(packet.sequence);

        if self.out.is_none() {
            let Some(ext) = extension(packet.payload_type) else {
                if self.packets == 1 {
                    warn!(
                        "Unsupported RTP payload type {}, stream not recorded",
                        packet.payload_type
                    );
                }
                return Ok(());
            };
            let path = self.prefix.with_extension(ext);
            info!(
                "Recording RTP payload type {} to {}",
                packet.payload_type,
                path.display()
            );
            self.out = Some((packet.payload_type, BufWriter::new(File::create(path)?)));
        }
        let (payload_type, out) = self.out.as_mut().unwrap();
        if packet.payload_type != *payload_type {
            debug!(
                "Ignoring RTP payload type {} on a {} stream",
                packet.payload_type, payload_type
            );
            return Ok(());
        }
        match *payload_type {
            PT_H264 => {
                for nal in self.depacketizer.push(packet.payload) {
                    out.write_all(&ANNEX_B_START_CODE)?;
                    out.write_all(&nal)?;
                    self.units += 1;
                }
            }
            _ => {
                out.write_all(packet.payload)?;
                self.units += 1;
            }
        }
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if let Some((_, out)) = self.out.as_mut() {
            out.flush()?;
        }
        Ok(())
    }
}

fn extension(payload_type: u8) -> Option<&'static str> {
    match payload_type {
        PT_H264 => Some("h264"),
        PT_MPEG4 => Some("m4v"),
        PT_PCMA => Some("alaw"),
        PT_PCMU => Some("ulaw"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp(pt: u8, seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80, pt];
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 1, 0xc0, 0x80, 0x9a, 0x0d]);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn parses_rtp_header() {
        let raw = rtp(PT_H264 | 0x80, 7, &[0x65, 1, 2]);
        let packet = RtpPacket::parse(&raw).unwrap();
        assert_eq!(packet.payload_type, PT_H264);
        assert!(packet.marker);
        assert_eq!(packet.sequence, 7);
        assert_eq!(packet.ssrc, 0xc0809a0d);
        assert_eq!(packet.payload, &[0x65, 1, 2]);
    }

    #[test]
    fn rejects_non_rtp() {
        assert!(RtpPacket::parse(&[0x00; 20]).is_none());
        assert!(RtpPacket::parse(&[0x80, 99, 0]).is_none());
    }

    #[test]
    fn strips_padding() {
        let mut raw = rtp(PT_PCMA, 1, &[1, 2, 3, 0, 0, 3]);
        raw[0] |= 0x20;
        assert_eq!(RtpPacket::parse(&raw).unwrap().payload, &[1, 2, 3]);
    }

    #[test]
    fn reassembles_fu_a() {
        let mut d = H264Depacketizer::default();
        // IDR slice (type 5, NRI 3) split in three fragments
        assert!(d.push(&[0x7c, 0x85, 1, 2]).is_empty());
        assert!(d.push(&[0x7c, 0x05, 3]).is_empty());
        assert_eq!(d.push(&[0x7c, 0x45, 4]), vec![vec![0x65, 1, 2, 3, 4]]);
    }

    #[test]
    fn drops_fragment_after_loss() {
        let mut d = H264Depacketizer::default();
        d.push(&[0x7c, 0x85, 1]);
        d.reset();
        assert!(d.push(&[0x7c, 0x45, 4]).is_empty());
    }

    #[test]
    fn splits_stap_a() {
        let mut d = H264Depacketizer::default();
        let nals = d.push(&[0x78, 0, 2, 0x67, 1, 0, 1, 0x68]);
        assert_eq!(nals, vec![vec![0x67, 1], vec![0x68]]);
    }

    #[test]
    fn records_h264_as_annex_b() {
        let dir = std::env::temp_dir().join(format!("viper-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut rec = StreamRecorder::new(dir.join("video"));
        for (seq, payload) in [
            (1u16, vec![0x67, 1]),
            (2, vec![0x68, 2]),
            (4, vec![0x65, 3]),
        ] {
            let raw = rtp(PT_H264, seq, &payload);
            rec.push(&RtpPacket::parse(&raw).unwrap()).unwrap();
        }
        rec.finish().unwrap();
        let path = rec.output().unwrap();
        assert_eq!(path, dir.join("video.h264"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            vec![
                0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3
            ]
        );
        assert_eq!((rec.packets, rec.lost, rec.units), (3, 1, 3));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

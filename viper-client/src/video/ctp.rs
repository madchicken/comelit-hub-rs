//! CTP transport and CSP call-signalling messages, carried on the CTPP channel.
//!
//! A CTP packet is:
//!
//! ```text
//! [flags] [0x18] [conn id (2)] [tx seq] [rx seq] [payload len u16 BE]
//! [payload, zero padded to 4] [ff ff ff ff] [src logaddr (10)] [dst logaddr (10)]
//! ```
//!
//! `0x18` is the size of the trailer. Flags: `0x80` opens the connection,
//! `0x40` marks a data packet that must be acknowledged, a packet without
//! `0x40` is a pure acknowledgement. The device answers with the connection id
//! having its top bit set. Each side numbers its own data packets (tx) and
//! acknowledges the peer by sending the next sequence it expects (rx).
//!
//! The payload is a CSP message whose first two bytes are its type (`00 01`
//! START, `00 11` MEDIAREQ, ...). Layouts come from the `csp_send_*` and
//! `csp_create_*` functions of `libvipcomelit.so` and match a capture of the
//! official app byte for byte.

use std::fmt;

pub const FLAG_OPEN: u8 = 0x80;
pub const FLAG_DATA: u8 = 0x40;
/// Closes the connection (`ctp_close`).
pub const FLAG_CLOSE: u8 = 0x20;

const TRAILER_LEN: u8 = 0x18;
const TRAILER_MAGIC: [u8; 4] = [0xff; 4];
const LEN_MASK: u16 = 0x07ff;

/// A Viper logical address: a NUL padded string of 10 bytes (e.g. "COMHUB012").
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LogAddr(pub [u8; 10]);

impl LogAddr {
    pub fn new(address: &str) -> Result<LogAddr, String> {
        let bytes = address.as_bytes();
        if bytes.len() > 9 {
            return Err(format!("address {address:?} is longer than 9 characters"));
        }
        let mut out = [0u8; 10];
        out[..bytes.len()].copy_from_slice(bytes);
        Ok(LogAddr(out))
    }
}

impl LogAddr {
    fn from_slice(bytes: &[u8]) -> LogAddr {
        let mut out = [0u8; 10];
        out.copy_from_slice(&bytes[..10]);
        LogAddr(out)
    }
}

impl fmt::Debug for LogAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let end = self.0.iter().position(|&b| b == 0).unwrap_or(10);
        write!(f, "{:?}", String::from_utf8_lossy(&self.0[..end]))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtpPacket {
    pub flags: u8,
    pub conn: [u8; 2],
    pub tx: u8,
    pub rx: u8,
    pub payload: Vec<u8>,
    pub src: LogAddr,
    pub dst: LogAddr,
}

impl CtpPacket {
    pub fn is_data(&self) -> bool {
        self.flags & FLAG_DATA != 0
    }

    pub fn encode(&self) -> Vec<u8> {
        let padded = self.payload.len().next_multiple_of(4);
        let mut out = Vec::with_capacity(8 + padded + TRAILER_LEN as usize);
        out.extend_from_slice(&[
            self.flags,
            TRAILER_LEN,
            self.conn[0],
            self.conn[1],
            self.tx,
            self.rx,
        ]);
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out.resize(8 + padded, 0);
        out.extend_from_slice(&TRAILER_MAGIC);
        out.extend_from_slice(&self.src.0);
        out.extend_from_slice(&self.dst.0);
        out
    }

    pub fn parse(body: &[u8]) -> Option<CtpPacket> {
        if body.len() < 8 || body[1] < TRAILER_LEN {
            return None;
        }
        let len = (u16::from_be_bytes([body[6], body[7]]) & LEN_MASK) as usize;
        let trailer = 8 + len.next_multiple_of(4);
        if body.len() < trailer + TRAILER_LEN as usize {
            return None;
        }
        let addr = |at: usize| {
            let mut a = [0u8; 10];
            a.copy_from_slice(&body[at..at + 10]);
            LogAddr(a)
        };
        Some(CtpPacket {
            flags: body[0],
            conn: [body[2], body[3]],
            tx: body[4],
            rx: body[5],
            payload: body[8..8 + len].to_vec(),
            src: addr(trailer + 4),
            dst: addr(trailer + 14),
        })
    }
}

/// CSP message types (first two bytes of a CTP payload).
pub mod kind {
    pub const START: u8 = 0x01;
    pub const CAPAB_REPORT: u8 = 0x03;
    pub const ALERTING: u8 = 0x0a;
    pub const CONNECT: u8 = 0x0c;
    pub const RELEASE: u8 = 0x0e;
    pub const MEDIAREQ: u8 = 0x11;
    pub const MEDIAOFFER: u8 = 0x12;
    pub const REGISTER_REQ: u8 = 0x40;
    pub const REGISTER_RES: u8 = 0x41;
}

/// MEDIAREQ action asking the peer to stream its audio to us.
pub const MEDIA_AUDIO: u8 = 0x18;
/// MEDIAREQ action asking the peer to stream its video to us.
pub const MEDIA_VIDEO: u8 = 0x14;
/// Capability bit announcing that a unit can send video.
pub const CAPAB_VIDEO: u32 = 0x08;

/// Unit type of an internal unit (the app presents itself as one).
pub const UNIT_INTERNAL: u8 = b'I';
/// Capabilities announced by the official app.
pub const APP_CAPABILITIES: u32 = 0x27;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Csp {
    /// Incoming call: `src` is the caller, `unit_type` its kind of unit.
    Start {
        src: LogAddr,
        dst: LogAddr,
        unit_type: u8,
    },
    CapabReport {
        unit_type: u8,
        capabilities: u32,
    },
    Alerting,
    Connect,
    Release {
        cause: u8,
    },
    MediaReq {
        action: u8,
        channel: u16,
    },
    MediaOffer,
    RegisterRes {
        id: u16,
        address: LogAddr,
        status: u8,
    },
    Other {
        kind: u8,
    },
}

impl Csp {
    pub fn parse(payload: &[u8]) -> Option<Csp> {
        if payload.len() < 2 || payload[0] != 0 {
            return None;
        }
        let msg = match payload[1] {
            kind::START if payload.len() >= 0x27 => Csp::Start {
                src: LogAddr::from_slice(&payload[2..12]),
                dst: LogAddr::from_slice(&payload[12..22]),
                unit_type: payload[0x26],
            },
            kind::CAPAB_REPORT if payload.len() >= 8 => Csp::CapabReport {
                unit_type: payload[2],
                capabilities: u32::from_le_bytes(payload[4..8].try_into().unwrap()),
            },
            kind::ALERTING => Csp::Alerting,
            kind::CONNECT => Csp::Connect,
            kind::RELEASE => Csp::Release {
                cause: payload.get(2).copied().unwrap_or(0),
            },
            kind::MEDIAREQ if payload.len() >= 10 => Csp::MediaReq {
                action: payload[2],
                channel: u16::from_le_bytes([payload[8], payload[9]]),
            },
            kind::MEDIAOFFER => Csp::MediaOffer,
            kind::REGISTER_RES if payload.len() >= 15 => Csp::RegisterRes {
                id: u16::from_le_bytes([payload[2], payload[3]]),
                address: LogAddr::from_slice(&payload[4..14]),
                status: payload[14],
            },
            other => Csp::Other { kind: other },
        };
        Some(msg)
    }

    /// START (40 bytes), as sent by `CallFsm::initBeginOutgoingCall`.
    pub fn start(src: &LogAddr, dst: &LogAddr, call_id: u32, unit_type: u8) -> Vec<u8> {
        let mut out = vec![0x00, kind::START];
        out.extend_from_slice(&src.0);
        out.extend_from_slice(&dst.0);
        out.extend_from_slice(&[0x01, 0x00]);
        out.extend_from_slice(&call_id.to_le_bytes());
        out.extend_from_slice(&src.0);
        out.extend_from_slice(&[unit_type, unit_type]);
        out
    }

    pub fn capab_report(unit_type: u8, capabilities: u32) -> Vec<u8> {
        let mut out = vec![0x00, kind::CAPAB_REPORT, unit_type, 0x00];
        out.extend_from_slice(&capabilities.to_le_bytes());
        out
    }

    /// Short MEDIAREQ: ask the peer to send `action` media to RTPC `channel`
    /// through the tunnel (flags 0x02, no IP address).
    pub fn media_request(action: u8, channel: u16) -> Vec<u8> {
        let mut out = vec![0x00, kind::MEDIAREQ, action, 0x02, 0, 0, 0, 0];
        out.extend_from_slice(&channel.to_le_bytes());
        out
    }

    /// Video MEDIAREQ (26 bytes), as sent by `CallFsm::start_videorx`.
    pub fn video_request(channel: u16, params: &VideoParams) -> Vec<u8> {
        let mut out = vec![0x00, kind::MEDIAREQ, MEDIA_VIDEO, 0x32, 0, 0, 0, 0];
        out.extend_from_slice(&channel.to_le_bytes());
        out.extend_from_slice(&params.max_rtp_payload.to_le_bytes());
        out.extend_from_slice(&params.bitrate_kbps.to_le_bytes());
        out.extend_from_slice(&params.width.to_le_bytes());
        out.extend_from_slice(&params.height.to_le_bytes());
        out.extend_from_slice(&params.width.to_le_bytes());
        out.extend_from_slice(&params.height.to_le_bytes());
        out.push(params.fps);
        out.push(0);
        out
    }

    pub fn alerting() -> Vec<u8> {
        vec![0x00, kind::ALERTING, 0x00]
    }

    /// REGISTER_REQ (17 bytes), sent by a subunit (`Subunit::su_go_registering`)
    /// to the apartment main address so that calls to the apartment are
    /// forwarded to it for `ttl_secs`.
    pub fn register_request(id: u16, address: &LogAddr, ttl_secs: u16) -> Vec<u8> {
        let mut out = vec![0x00, kind::REGISTER_REQ];
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&address.0);
        out.extend_from_slice(&ttl_secs.to_le_bytes());
        out.push(0);
        out
    }

    pub fn release(cause: u8) -> Vec<u8> {
        vec![0x00, kind::RELEASE, cause]
    }
}

/// Video stream parameters requested in the video MEDIAREQ. Defaults are the
/// values the official app sent in the reference capture.
#[derive(Debug, Clone, Copy)]
pub struct VideoParams {
    pub max_rtp_payload: u16,
    pub bitrate_kbps: u32,
    pub width: u16,
    pub height: u16,
    pub fps: u8,
}

impl Default for VideoParams {
    fn default() -> Self {
        VideoParams {
            max_rtp_payload: 1300,
            bitrate_kbps: 1000,
            width: 1024,
            height: 608,
            fps: 16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    fn me() -> LogAddr {
        LogAddr::new("COMHUB012").unwrap()
    }

    fn door() -> LogAddr {
        LogAddr::new("00000100").unwrap()
    }

    fn data(tx: u8, rx: u8, payload: Vec<u8>) -> CtpPacket {
        CtpPacket {
            flags: FLAG_DATA,
            conn: [0x4e, 0xf3],
            tx,
            rx,
            payload,
            src: me(),
            dst: door(),
        }
    }

    #[test]
    fn start_matches_capture() {
        // CTP body of pkt93
        let expected = hex(
            "c0 18 4e f3 b1 0c 00 28 00 01 43 4f 4d 48 55 42 30 31 32 00 30 30 30 30 30 31 30 30 \
             00 00 01 00 84 35 cd 32 43 4f 4d 48 55 42 30 31 32 00 49 49 ff ff ff ff 43 4f 4d 48 \
             55 42 30 31 32 00 30 30 30 30 30 31 30 30 00 00",
        );
        let packet = CtpPacket {
            flags: FLAG_OPEN | FLAG_DATA,
            ..data(
                0xb1,
                0x0c,
                Csp::start(&me(), &door(), 0x32cd3584, UNIT_INTERNAL),
            )
        };
        assert_eq!(packet.encode(), expected);
    }

    #[test]
    fn capab_report_matches_capture() {
        // CTP body of pkt111
        let expected = hex(
            "40 18 4e f3 b2 0c 00 08 00 03 49 00 27 00 00 00 ff ff ff ff 43 4f 4d 48 55 42 30 31 \
             32 00 30 30 30 30 30 31 30 30 00 00",
        );
        let packet = data(
            0xb2,
            0x0c,
            Csp::capab_report(UNIT_INTERNAL, APP_CAPABILITIES),
        );
        assert_eq!(packet.encode(), expected);
    }

    #[test]
    fn audio_request_matches_capture() {
        // CTP body of pkt154 (payload padded from 10 to 12 bytes)
        let expected = hex(
            "40 18 4e f3 b3 0e 00 0a 00 11 18 02 00 00 00 00 7c 51 00 00 ff ff ff ff 43 4f 4d 48 \
             55 42 30 31 32 00 30 30 30 30 30 31 30 30 00 00",
        );
        let packet = data(0xb3, 0x0e, Csp::media_request(MEDIA_AUDIO, 0x517c));
        assert_eq!(packet.encode(), expected);
    }

    #[test]
    fn video_request_matches_capture() {
        // CTP body of pkt198
        let expected = hex(
            "40 18 4e f3 b4 0f 00 1a 00 11 14 32 00 00 00 00 7d 51 14 05 e8 03 00 00 00 04 60 02 \
             00 04 60 02 10 00 00 00 ff ff ff ff 43 4f 4d 48 55 42 30 31 32 00 30 30 30 30 30 31 \
             30 30 00 00",
        );
        let payload = Csp::video_request(0x517d, &VideoParams::default());
        assert_eq!(data(0xb4, 0x0f, payload).encode(), expected);
    }

    #[test]
    fn parses_device_capab_report() {
        // CTP body of pkt128
        let raw = hex(
            "40 18 ce f3 0c b3 00 08 00 03 50 07 1b 00 00 00 ff ff ff ff 30 30 30 30 30 31 30 30 \
             00 00 43 4f 4d 48 55 42 30 31 32 00",
        );
        let packet = CtpPacket::parse(&raw).unwrap();
        assert!(packet.is_data());
        assert_eq!(
            (packet.conn, packet.tx, packet.rx),
            ([0xce, 0xf3], 0x0c, 0xb3)
        );
        assert_eq!(packet.src, door());
        assert_eq!(packet.dst, me());
        assert_eq!(
            Csp::parse(&packet.payload),
            Some(Csp::CapabReport {
                unit_type: b'P',
                capabilities: 0x1b,
            })
        );
    }

    #[test]
    fn parses_device_ack_and_connect() {
        // pkt109 (pure ack) and pkt140 (CONNECT)
        let ack = CtpPacket::parse(&hex(
            "00 18 ce f3 0c b2 00 00 ff ff ff ff 30 30 30 30 30 31 30 30 00 00 43 4f 4d 48 55 42 \
             30 31 32 00",
        ))
        .unwrap();
        assert!(!ack.is_data());
        assert_eq!(ack.rx, 0xb2);
        assert!(ack.payload.is_empty());

        let connect = CtpPacket::parse(&hex(
            "40 18 ce f3 0d b3 00 02 00 0c 00 00 ff ff ff ff 30 30 30 30 30 31 30 30 00 00 43 4f \
             4d 48 55 42 30 31 32 00",
        ))
        .unwrap();
        assert_eq!(Csp::parse(&connect.payload), Some(Csp::Connect));
    }

    #[test]
    fn parses_device_media_request() {
        assert_eq!(
            Csp::parse(&hex("00 11 18 02 00 00 00 00 9e ef")),
            Some(Csp::MediaReq {
                action: MEDIA_AUDIO,
                channel: 0xef9e,
            })
        );
    }

    #[test]
    fn parses_incoming_start() {
        let payload = Csp::start(&door(), &LogAddr::new("COMHUB01").unwrap(), 7, b'P');
        assert_eq!(
            Csp::parse(&payload),
            Some(Csp::Start {
                src: door(),
                dst: LogAddr::new("COMHUB01").unwrap(),
                unit_type: b'P',
            })
        );
    }

    #[test]
    fn register_request_layout() {
        // Subunit::su_go_registering: id, own address, ttl 3600 s, flags 0
        let req = Csp::register_request(0xbeef, &me(), 0x0e10);
        assert_eq!(req.len(), 17);
        assert_eq!(&req[..4], &[0x00, 0x40, 0xef, 0xbe]);
        assert_eq!(&req[4..14], &me().0);
        assert_eq!(&req[14..], &[0x10, 0x0e, 0x00]);

        let mut res = vec![0x00, 0x41, 0xef, 0xbe];
        res.extend_from_slice(&me().0);
        res.extend_from_slice(&[0x00, 0x00]);
        assert_eq!(
            Csp::parse(&res),
            Some(Csp::RegisterRes {
                id: 0xbeef,
                address: me(),
                status: 0,
            })
        );
    }

    #[test]
    fn rejects_long_addresses() {
        assert!(LogAddr::new("0123456789").is_err());
    }
}

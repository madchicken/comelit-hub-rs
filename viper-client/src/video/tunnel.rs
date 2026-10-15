//! Viper tunnel framing, shared by the TCP and UDP transports (port 64100).
//!
//! Every frame starts with an 8 byte header:
//!
//! ```text
//! [flags & 3] [0x06] [body len u16 LE] [channel id u16 LE] [00 00]
//! ```
//!
//! Channel 0 is the control channel used to open/close the logical channels
//! (UDPM, CTPP, RTPC, ...). Reverse engineered from `viper_channel_send` and
//! `viper_tunnel_channel_open` in `libvipcomelit.so`.

pub const HEADER_LEN: usize = 8;
pub const CONTROL_CHANNEL: u16 = 0;

const CONTROL_MAGIC: [u8; 2] = [0xcd, 0xab];
const CLOSE_MAGIC: [u8; 2] = [0xef, 0x01];
const CONTROL_OPEN: u16 = 1;
const CONTROL_OPEN_RESPONSE: u16 = 2;
const CONTROL_CLOSE: u16 = 3;

/// Transport requested when opening a channel. Only RTPC uses `Udp`: its
/// frames move to UDP once the UDPM lane is open, otherwise they stay on TCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Transport {
    Tcp = 0,
    Udp = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub channel: u16,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn encode(channel: u16, body: &[u8]) -> Vec<u8> {
        let len = (body.len() as u16).to_le_bytes();
        let ch = channel.to_le_bytes();
        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        out.extend_from_slice(&[0x00, 0x06, len[0], len[1], ch[0], ch[1], 0x00, 0x00]);
        out.extend_from_slice(body);
        out
    }

    /// Decodes one frame from the front of `buf`, returning it with the number
    /// of bytes consumed, or `None` if `buf` does not hold a whole frame yet.
    pub fn decode(buf: &[u8]) -> Option<(Frame, usize)> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let len = u16::from_le_bytes([buf[2], buf[3]]) as usize;
        if buf.len() < HEADER_LEN + len {
            return None;
        }
        let frame = Frame {
            channel: u16::from_le_bytes([buf[4], buf[5]]),
            body: buf[HEADER_LEN..HEADER_LEN + len].to_vec(),
        };
        Some((frame, HEADER_LEN + len))
    }
}

/// Messages exchanged on the control channel (channel 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// `cd ab 01 00 07 00 00 00 <name> <id> <transport> [00 <len u16> 00 00 <extra>]`
    Open {
        name: [u8; 4],
        channel: u16,
        transport: u8,
        extra: Vec<u8>,
    },
    /// `cd ab 02 00 04 00 00 00 <id> <status> [<len u16> 00 00 <extra>]`
    OpenResponse {
        channel: u16,
        status: u16,
        extra: Vec<u8>,
    },
    /// `ef 01 03 00 02 00 00 00 <id>`
    Close { channel: u16 },
}

impl Control {
    pub fn open(name: &str, channel: u16, transport: Transport, extra: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(20 + extra.len());
        body.extend_from_slice(&CONTROL_MAGIC);
        body.extend_from_slice(&CONTROL_OPEN.to_le_bytes());
        body.extend_from_slice(&[0x07, 0x00, 0x00, 0x00]);
        body.extend_from_slice(&name.as_bytes()[..4]);
        body.extend_from_slice(&channel.to_le_bytes());
        body.push(transport as u8);
        if !extra.is_empty() {
            body.push(0x00);
            body.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            body.extend_from_slice(&[0x00, 0x00]);
            body.extend_from_slice(extra);
        }
        Frame::encode(CONTROL_CHANNEL, &body)
    }

    /// Accepts (status 0) a channel the device opened towards us.
    pub fn open_response(channel: u16, status: u16) -> Vec<u8> {
        let mut body = Vec::with_capacity(12);
        body.extend_from_slice(&CONTROL_MAGIC);
        body.extend_from_slice(&CONTROL_OPEN_RESPONSE.to_le_bytes());
        body.extend_from_slice(&[0x04, 0x00, 0x00, 0x00]);
        body.extend_from_slice(&channel.to_le_bytes());
        body.extend_from_slice(&status.to_le_bytes());
        Frame::encode(CONTROL_CHANNEL, &body)
    }

    pub fn close(channel: u16) -> Vec<u8> {
        let mut body = Vec::with_capacity(10);
        body.extend_from_slice(&CLOSE_MAGIC);
        body.extend_from_slice(&CONTROL_CLOSE.to_le_bytes());
        body.extend_from_slice(&[0x02, 0x00, 0x00, 0x00]);
        body.extend_from_slice(&channel.to_le_bytes());
        Frame::encode(CONTROL_CHANNEL, &body)
    }

    pub fn parse(body: &[u8]) -> Option<Control> {
        if body.len() < 10 {
            return None;
        }
        let kind = u16::from_le_bytes([body[2], body[3]]);
        let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
        // Optional trailing data: <len u16> 00 00 <data>, starting at `at`.
        let extra_at = |at: usize| -> Vec<u8> {
            if body.len() < at + 4 {
                return vec![];
            }
            let len = u16_at(at) as usize;
            body[at + 4..body.len().min(at + 4 + len)].to_vec()
        };
        match kind {
            CONTROL_OPEN if body.len() >= 15 => Some(Control::Open {
                name: [body[8], body[9], body[10], body[11]],
                channel: u16_at(12),
                transport: body[14],
                extra: extra_at(16),
            }),
            CONTROL_OPEN_RESPONSE if body.len() >= 12 => Some(Control::OpenResponse {
                channel: u16_at(8),
                status: u16_at(10),
                extra: extra_at(12),
            }),
            CONTROL_CLOSE => Some(Control::Close { channel: u16_at(8) }),
            _ => None,
        }
    }
}

/// UDPM lane keepalive, 6 bytes carried on the UDPM channel over UDP:
///
/// ```text
/// [token (2)] [is_reply] [seq] [lane_open] [0x80]
/// ```
///
/// The token is the trailing data of the UDPM open response. The client pings
/// with `is_reply = 0`; the device answers with `is_reply = 1, lane_open = 1`,
/// at which point the lane is open and RTPC traffic flows over UDP. Pings from
/// the device must be answered the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpmPing {
    pub token: [u8; 2],
    pub is_reply: bool,
    pub seq: u8,
    pub lane_open: bool,
}

impl UdpmPing {
    pub fn encode(&self) -> [u8; 6] {
        [
            self.token[0],
            self.token[1],
            self.is_reply as u8,
            self.seq,
            self.lane_open as u8,
            0x80,
        ]
    }

    pub fn parse(body: &[u8]) -> Option<UdpmPing> {
        if body.len() < 6 {
            return None;
        }
        Some(UdpmPing {
            token: [body[0], body[1]],
            is_reply: body[2] != 0,
            seq: body[3],
            lane_open: body[4] & 1 != 0,
        })
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

    #[test]
    fn encodes_rtpc_open_like_the_app() {
        // pkt143 of the app capture
        let expected = hex("00 06 0f 00 00 00 00 00 cd ab 01 00 07 00 00 00 52 54 50 43 7c 51 01");
        assert_eq!(Control::open("RTPC", 0x517c, Transport::Udp, &[]), expected);
    }

    #[test]
    fn ctpp_open_matches_legacy_command_encoding() {
        let legacy = crate::command::Command::channel(
            &"CTPP".to_string(),
            &[0x74, 0x51],
            Some(b"COMHUB012"),
            None,
        );
        assert_eq!(
            Control::open("CTPP", 0x5174, Transport::Tcp, b"COMHUB012\0"),
            legacy
        );
    }

    #[test]
    fn parses_udpm_open_response_token() {
        // pkt101 of the app capture
        let raw =
            hex("00 06 12 00 00 00 00 00 cd ab 02 00 04 00 00 00 7b 51 00 00 02 00 00 00 6b 12");
        let (frame, used) = Frame::decode(&raw).unwrap();
        assert_eq!(used, raw.len());
        assert_eq!(frame.channel, CONTROL_CHANNEL);
        assert_eq!(
            Control::parse(&frame.body),
            Some(Control::OpenResponse {
                channel: 0x517b,
                status: 0,
                extra: vec![0x6b, 0x12],
            })
        );
    }

    #[test]
    fn parses_device_initiated_open_and_answers_it() {
        // pkt146 / pkt148 of the app capture
        let raw = hex("cd ab 01 00 07 00 00 00 52 54 50 43 9e ef 01");
        assert_eq!(
            Control::parse(&raw),
            Some(Control::Open {
                name: *b"RTPC",
                channel: 0xef9e,
                transport: 1,
                extra: vec![],
            })
        );
        assert_eq!(
            Control::open_response(0xef9e, 0),
            hex("00 06 0c 00 00 00 00 00 cd ab 02 00 04 00 00 00 9e ef 00 00")
        );
    }

    #[test]
    fn close_matches_legacy_command_encoding() {
        assert_eq!(
            Control::close(0x5174),
            crate::command::Command::close(&[0x74, 0x51])
        );
    }

    #[test]
    fn frame_decode_waits_for_whole_frame() {
        let raw = Frame::encode(0x517d, &[1, 2, 3, 4]);
        assert!(Frame::decode(&raw[..raw.len() - 1]).is_none());
        let (frame, used) = Frame::decode(&raw).unwrap();
        assert_eq!(used, 12);
        assert_eq!(frame.body, vec![1, 2, 3, 4]);
    }

    #[test]
    fn udpm_ping_round_trip() {
        let ping = UdpmPing {
            token: [0x6b, 0x12],
            is_reply: false,
            seq: 0x2a,
            lane_open: false,
        };
        assert_eq!(ping.encode(), [0x6b, 0x12, 0x00, 0x2a, 0x00, 0x80]);
        let reply = UdpmPing::parse(&[0x6b, 0x12, 0x01, 0x2a, 0x01, 0x80]).unwrap();
        assert!(reply.is_reply && reply.lane_open);
    }
}

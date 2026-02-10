/* Protocol packet structures for the custom RTP protocol

 This module defines all packet types based on the protocol analysis:
 1. TCP Control Channel - "RTPC" handshake
 2. UDP Init Handshake - 0x00 0x06 0x06 0x00
 3. RTP Video/Audio Data - 0x00 0x06 0x0d 0x04 / 0x00 0x06 0xac 0x00
 4. RTCP Feedback - 0x00 0x06 0xac 0x00
*/
#![allow(dead_code)]
/// Protocol magic header - always 0x00 0x06
pub const PROTOCOL_MAGIC: u16 = 0x0006;

/// Message types for the custom protocol
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum MessageType {
    Unknown = 0x4800,
    /// TCP control handshake (0x0f 0x00)
    TcpControl = 0x0f00,
    /// UDP initialization (0x06 0x00)
    UdpInit = 0x0600,
    /// RTP video data (0x0d 0x04)
    RtpVideo = 0x0d04,
    /// RTCP feedback (0xac 0x00)
    RtcpFeedback = 0xac00,
}

impl MessageType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            0x4800 => Some(MessageType::Unknown),
            0x0f00 => Some(MessageType::TcpControl),
            0x0600 => Some(MessageType::UdpInit),
            0x0d04 => Some(MessageType::RtpVideo),
            0xac00 => Some(MessageType::RtcpFeedback),
            _ => None,
        }
    }

    pub fn to_bytes(&self) -> [u8; 2] {
        let value = *self as u16;
        [(value >> 8) as u8, value as u8]
    }
}

/// Custom protocol header (8 bytes)
///
/// Structure:
/// ```
/// 0x00 0x06                    - Protocol magic
/// [MessageType: 2 bytes]       - Message type
/// [SessionID: 2 bytes]         - Session identifier
/// 0x00 0x00                    - Reserved/padding
/// ```
#[derive(Debug, Clone)]
pub struct CustomHeader {
    pub message_type: MessageType,
    pub session_id: u16,
}

impl CustomHeader {
    pub fn new(message_type: MessageType, session_id: u16) -> Self {
        Self {
            message_type,
            session_id,
        }
    }

    /// Serialize to bytes (8 bytes total)
    pub fn to_bytes(&self) -> [u8; 8] {
        let mut bytes = [0u8; 8];

        // Protocol magic (0x00 0x06)
        bytes[0] = 0x00;
        bytes[1] = 0x06;

        // Message type (2 bytes)
        let msg_bytes = self.message_type.to_bytes();
        bytes[2] = msg_bytes[0];
        bytes[3] = msg_bytes[1];

        // Session ID (2 bytes, big-endian)
        bytes[4] = (self.session_id >> 8) as u8;
        bytes[5] = self.session_id as u8;

        // Reserved (0x00 0x00)
        bytes[6] = 0x00;
        bytes[7] = 0x00;

        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 8 {
            return Err("Header too short");
        }

        // Check protocol magic
        if bytes[0] != 0x00 || bytes[1] != 0x06 {
            return Err("Invalid protocol magic");
        }

        // Parse message type
        let msg_type_raw = ((bytes[2] as u16) << 8) | (bytes[3] as u16);
        let message_type = MessageType::from_u16(msg_type_raw).ok_or("Unknown message type")?;

        // Parse session ID
        let session_id = ((bytes[4] as u16) << 8) | (bytes[5] as u16);

        Ok(Self {
            message_type,
            session_id,
        })
    }
}

/// Standard RTP header (12 bytes minimum)
#[derive(Debug, Clone)]
pub struct RtpHeader {
    pub version: u8,      // 2 bits (always 2)
    pub padding: bool,    // 1 bit
    pub extension: bool,  // 1 bit
    pub csrc_count: u8,   // 4 bits
    pub marker: bool,     // 1 bit
    pub payload_type: u8, // 7 bits
    pub sequence: u16,    // 16 bits
    pub timestamp: u32,   // 32 bits
    pub ssrc: u32,        // 32 bits
}

impl RtpHeader {
    pub fn new(payload_type: u8, sequence: u16, timestamp: u32, ssrc: u32) -> Self {
        Self {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type,
            sequence,
            timestamp,
            ssrc,
        }
    }

    /// Serialize to bytes (12 bytes)
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];

        // Byte 0: V(2), P(1), X(1), CC(4)
        bytes[0] = ((self.version & 0x03) << 6)
            | ((self.padding as u8) << 5)
            | ((self.extension as u8) << 4)
            | (self.csrc_count & 0x0F);

        // Byte 1: M(1), PT(7)
        bytes[1] = ((self.marker as u8) << 7) | (self.payload_type & 0x7F);

        // Bytes 2-3: Sequence number (big-endian)
        bytes[2] = (self.sequence >> 8) as u8;
        bytes[3] = self.sequence as u8;

        // Bytes 4-7: Timestamp (big-endian)
        bytes[4] = (self.timestamp >> 24) as u8;
        bytes[5] = (self.timestamp >> 16) as u8;
        bytes[6] = (self.timestamp >> 8) as u8;
        bytes[7] = self.timestamp as u8;

        // Bytes 8-11: SSRC (big-endian)
        bytes[8] = (self.ssrc >> 24) as u8;
        bytes[9] = (self.ssrc >> 16) as u8;
        bytes[10] = (self.ssrc >> 8) as u8;
        bytes[11] = self.ssrc as u8;

        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 12 {
            return Err("RTP header too short");
        }

        let version = (bytes[0] >> 6) & 0x03;
        let padding = ((bytes[0] >> 5) & 0x01) != 0;
        let extension = ((bytes[0] >> 4) & 0x01) != 0;
        let csrc_count = bytes[0] & 0x0F;

        let marker = ((bytes[1] >> 7) & 0x01) != 0;
        let payload_type = bytes[1] & 0x7F;

        let sequence = ((bytes[2] as u16) << 8) | (bytes[3] as u16);

        let timestamp = ((bytes[4] as u32) << 24)
            | ((bytes[5] as u32) << 16)
            | ((bytes[6] as u32) << 8)
            | (bytes[7] as u32);

        let ssrc = ((bytes[8] as u32) << 24)
            | ((bytes[9] as u32) << 16)
            | ((bytes[10] as u32) << 8)
            | (bytes[11] as u32);

        Ok(Self {
            version,
            padding,
            extension,
            csrc_count,
            marker,
            payload_type,
            sequence,
            timestamp,
            ssrc,
        })
    }
}

/// TCP Control packet - "RTPC" handshake
///
/// Example from your capture:
/// ```
/// 00 06 0f 00 00 00 00 00 cd ab 01 00 07 00 00 00 52 54 50 43 49 75 01
/// ```
#[derive(Debug, Clone)]
pub struct TcpControlPacket {
    pub header: CustomHeader,
    pub unknown1: u32,     // 0xcdab0100 (likely magic or version)
    pub payload_len: u32,  // Length of following data (7)
    pub protocol: [u8; 4], // "RTPC" ASCII
    pub params: Vec<u8>,   // Additional parameters
}

impl TcpControlPacket {
    pub fn new(session_id: u16) -> Self {
        Self {
            header: CustomHeader::new(MessageType::TcpControl, session_id),
            unknown1: 0x0100abcd, // Note: might be little-endian
            payload_len: 7,
            protocol: *b"RTPC",
            params: vec![0x49, 0x75, 0x01],
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Custom header (8 bytes)
        bytes.extend_from_slice(&self.header.to_bytes());

        // Unknown magic (4 bytes, little-endian based on 'cd ab')
        bytes.extend_from_slice(&self.unknown1.to_le_bytes());

        // Payload length (4 bytes, little-endian)
        bytes.extend_from_slice(&self.payload_len.to_le_bytes());

        // Protocol name (4 bytes)
        bytes.extend_from_slice(&self.protocol);

        // Additional parameters
        bytes.extend_from_slice(&self.params);

        bytes
    }
}

/// UDP Init packet - Session handshake
///
/// Client sends:
/// ```
/// 00 06 06 00 48 75 00 00 3a 12 00 67 00 80
/// ```
/// Server responds:
/// ```
/// 00 06 06 00 48 75 00 00 3a 12 01 69 01 80 00 00 00 00
/// ```
#[derive(Debug, Clone)]
pub struct UdpInitPacket {
    pub header: CustomHeader,
    pub param1: u16,    // 0x3a12 (14866) - could be MTU, port, or version
    pub param2: u16,    // Client: 0x0067 (103), Server: 0x0169 (361)
    pub param3: u16,    // Client: 0x0080 (128), Server: 0x0180 (384)
    pub extra: Vec<u8>, // Server adds 4 extra bytes
}

impl UdpInitPacket {
    pub fn new_client_request(session_id: u16) -> Self {
        Self {
            header: CustomHeader::new(MessageType::UdpInit, session_id),
            param1: 0x3a12, // Common parameter
            param2: 0x0067, // Client proposal
            param3: 0x0080, // Client capability
            extra: vec![],  // No extra bytes in request
        }
    }

    pub fn new_server_response(session_id: u16) -> Self {
        Self {
            header: CustomHeader::new(MessageType::UdpInit, session_id),
            param1: 0x3a12,                      // Same as client
            param2: 0x0169,                      // Server acceptance
            param3: 0x0180,                      // Server capability
            extra: vec![0x00, 0x00, 0x00, 0x00], // Server adds padding
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Custom header (8 bytes)
        bytes.extend_from_slice(&self.header.to_bytes());

        // Parameters (big-endian based on packet analysis)
        bytes.extend_from_slice(&self.param1.to_be_bytes());
        bytes.extend_from_slice(&self.param2.to_be_bytes());
        bytes.extend_from_slice(&self.param3.to_be_bytes());

        // Extra bytes (server response only)
        bytes.extend_from_slice(&self.extra);

        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 14 {
            return Err("UDP init packet too short");
        }

        let header = CustomHeader::from_bytes(&bytes[0..8])?;

        let param1 = ((bytes[8] as u16) << 8) | (bytes[9] as u16);
        let param2 = ((bytes[10] as u16) << 8) | (bytes[11] as u16);
        let param3 = ((bytes[12] as u16) << 8) | (bytes[13] as u16);

        let extra = if bytes.len() > 14 {
            bytes[14..].to_vec()
        } else {
            vec![]
        };

        Ok(Self {
            header,
            param1,
            param2,
            param3,
            extra,
        })
    }
}

/// Complete RTP packet with custom header
#[derive(Debug, Clone)]
pub struct RtpPacket {
    pub custom_header: CustomHeader,
    pub rtp_header: RtpHeader,
    pub payload: Vec<u8>,
}

impl RtpPacket {
    pub fn new(
        session_id: u16,
        payload_type: u8,
        sequence: u16,
        timestamp: u32,
        ssrc: u32,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            custom_header: CustomHeader::new(MessageType::RtpVideo, session_id),
            rtp_header: RtpHeader::new(payload_type, sequence, timestamp, ssrc),
            payload,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Custom header (8 bytes)
        bytes.extend_from_slice(&self.custom_header.to_bytes());

        // RTP header (12 bytes)
        bytes.extend_from_slice(&self.rtp_header.to_bytes());

        // Payload
        bytes.extend_from_slice(&self.payload);

        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 20 {
            return Err("RTP packet too short");
        }

        let custom_header = CustomHeader::from_bytes(&bytes[0..8])?;
        let rtp_header = RtpHeader::from_bytes(&bytes[8..20])?;
        let payload = bytes[20..].to_vec();

        Ok(Self {
            custom_header,
            rtp_header,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_header_serialization() {
        let header = CustomHeader::new(MessageType::UdpInit, 0x4875);
        let bytes = header.to_bytes();

        assert_eq!(bytes[0], 0x00);
        assert_eq!(bytes[1], 0x06);
        assert_eq!(bytes[2], 0x06);
        assert_eq!(bytes[3], 0x00);
        assert_eq!(bytes[4], 0x48);
        assert_eq!(bytes[5], 0x75);

        let decoded = CustomHeader::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.session_id, 0x4875);
    }

    #[test]
    fn test_udp_init_packet() {
        let packet = UdpInitPacket::new_client_request(0x4875);
        let bytes = packet.to_bytes();

        // Should match the client packet from capture
        assert_eq!(bytes.len(), 14);
        assert_eq!(&bytes[0..2], &[0x00, 0x06]);
        assert_eq!(&bytes[2..4], &[0x06, 0x00]);
        assert_eq!(&bytes[4..6], &[0x48, 0x75]);
    }

    #[test]
    fn test_rtp_header_serialization() {
        let header = RtpHeader::new(99, 0x1211, 0xfd6e3a0b, 0xc0809a0d);
        let bytes = header.to_bytes();

        assert_eq!(bytes[0], 0x80); // Version 2, no padding/extension
        assert_eq!(bytes[1], 99); // Payload type

        let decoded = RtpHeader::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.version, 2);
        assert_eq!(decoded.payload_type, 99);
        assert_eq!(decoded.sequence, 0x1211);
    }
}

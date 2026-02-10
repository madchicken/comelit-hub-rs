# Doorbell Custom Protocol Analysis (CORRECTED)

## Overview
This document analyzes a proprietary protocol used by a doorbell system. The capture shows communication to turn on the doorbell's camera.

## Network Configuration

**Devices:**
- **Client/Controller**: 192.168.0.126
- **Doorbell Device**: 192.168.0.66
- **Device Identifier**: COMHUB01 (apt-address)
- **User Identifier**: 2 (apt-subaddress)
- **Open Door APT address**: 00000100

**Ports:**
- **Primary Control**: TCP 64100 (command/control protocol)
- **Video Stream**: UDP 64100 (video data packets - same port!)
- **Encrypted Signaling**: TCP 7000 (appears to be key exchange/encrypted control)
- **Secondary Control**: TCP 10012

## Protocol Structure (All Little Endian)

### Header Format

```
Offset  Size  Description                    Example
------  ----  -----------                    -------
0x00    2     Magic/Version (Big Endian)     0x0006
0x02    2     Message Length (LE)            0x0048 = 72 bytes
0x04    2     Session ID (LE)                0x5174 or 0x0000
0x06    2     Flags (LE)                     0x0000
0x08    4     Context Data (LE)              varies by message type
0x0C    ...   Payload
```

**Key Insight**: Bytes at 0x04-0x05 are a **Session ID in Little Endian format**.
- Written as `74 51` in hex dump
- Reads as `0x5174` when interpreted as little endian
- Your observation is correct - this increments when new channels open!

## Session Management

### Discovered Sessions

#### Session 0x0000 - Control/Setup Channel
- Used for UDPM (UDP Mode) setup
- Used for RTPC (RTP Control) messages
- Uses special context format:
  ```
  Bytes 0x08-0x0b: 0xcd 0xab 0x01 0x00
                   ^^^^^^^ ^^^^^^^
                   Magic   Message Type
  ```
  - Magic: `0xabcd` (marks control messages)
  - Type: `0x01` = Request, `0x02` = Response/ACK

#### Session 0x5174 (20852) - Device Communication Channel
- Main session for device info and commands
- Contains device identifiers (COMHUB01, 2, 00000100)
- Used for camera control commands
- Context data varies but includes sequence numbers

### Session Pattern

Your hypothesis is **CONFIRMED**: The session ID increments when opening new logical channels:
1. Session 0x5174 opens first (device registration/main channel)
2. Session 0x0000 used for protocol control messages (UDPM, RTPC)

The session ID likely increments per connection or logical stream, though we only see two sessions in this capture.

## Message Types by Context Data

### Control Messages (Session 0x0000)

Format: `cd ab [type] 00 [length] 00 00 00 [data]`

**Type 0x01 - Channel Open Request**
```
Packet #94 (UDPM):
00 06 0f 00 | 00 00 00 00 | cd ab 01 00 | 07 00 00 00 | 55 44 50 4d 7b 51 01
            |  Session 0  |  Magic+Type |   Length    | "UDPM" + ID

Packet #143 (RTPC):
00 06 0f 00 | 00 00 00 00 | cd ab 01 00 | 07 00 00 00 | 52 54 50 43 7c 51 01
            |  Session 0  |  Magic+Type |   Length    | "RTPC" + ID
```

Channel types:
- **UDPM**: UDP Mode - sets up UDP communication
- **RTPC**: RTP Control - real-time protocol control
- **CCTP**: (not seen in this capture, but likely exists)

**Type 0x02 - Acknowledgment**
```
Packet #101:
00 06 12 00 | 00 00 00 00 | cd ab 02 00 | 04 00 00 00 | 7b 51 00 00
            |  Session 0  | ACK type    |   Length    | Response data
```

### Device Messages (Session 0x5174)

Context format: `[flags] [seq1] [seq2] [type] [cmd] [subcmd] ...`

Different context patterns:
- `c0 18 4e f3` - Client messages with bit 0x40 set
- `00 18 ce f3` - Device responses
- `40 18 4e f3` - Client commands with flag

Sequence tracking at offsets 0x0c-0x0d:
- `b1 0c`, `b2 0c`, `b3 0d`, etc. (incrementing counters)

## Camera Control Protocol

### Packet Structure Breakdown

**Initial Registration (Packet #93)**
```
00 06    Magic: 0x0006
48 00    Length: 72 bytes (LE)
74 51    Session: 0x5174 (LE)
00 00    Flags
c0 18 4e f3    Context (client message marker)
b1 0c    Sequence
00 28    Message type
00 01    Subtype
43 4f 4d 48 55 42 30 31 32 00    "COMHUB012"
30 30 30 30 30 31 30 30 00 00    "00000100"
...more device info...
```

**Camera Start Command (Packet #154)**
```
00 06    Magic
2c 00    Length: 44 bytes (LE)
74 51    Session: 0x5174 (LE)
00 00    Flags
40 18 4e f3    Context (command flag)
b3 0e    Sequence
00 0a    Message type: 0x0a
00 11    Command class: 0x11 (VIDEO)
18       Subcommand: 0x18 (START)
02       Parameter: 0x02 (mode/camera ID)
00 00 00 00    Reserved
7c 51 00 00    Reference ID (from RTPC handshake)
ff ff ff ff    Separator
43 4f 4d 48 55 42 30 31 32 00    "COMHUB012"
30 30 30 30 30 31 30 30 00 00    "00000100"
```

**Camera Configuration (Packet #198)**
```
00 06    Magic
3c 00    Length: 60 bytes (LE)
74 51    Session: 0x5174 (LE)
00 00    Flags
40 18 4e f3    Context
b4 0f    Sequence
00 1a    Message type: 0x1a
00 11    Command class: 0x11 (VIDEO)
14       Subcommand: 0x14 (CONFIGURE)
32       Parameter: 50 (decimal)
00 00 00 00    Reserved
7d 51    Reference ID
14 05    Config param 1
e8 03    Config param 2: 1000 (0x03e8 LE = bitrate in kbps?)
00 00    
00 04    Resolution width low bytes: 1024?
60 02    Resolution related: 608?
00 04    Resolution related
60 02    Resolution related
10 00 00 00    Config flags
ff ff ff ff    Separator
43 4f 4d...    Device IDs
```

## Complete Camera Activation Sequence

1. **Session Establishment** (Packet #93)
   - TCP to 192.168.0.66:64100
   - Session 0x5174 created
   - Device COMHUB012 registered
   - Serial 00000100 sent

2. **UDP Mode Setup** (Packet #94)
   - TCP control message
   - Session 0x0000 control message
   - "UDPM" channel opened with ID 0x517b
   - ACK received (Packet #101)

3. **Device Status Exchange** (Packets #109-142)
   - TCP on session 0x5174
   - Periodic status messages
   - Keep-alive with device identifiers
   - Sequence numbers increment: b1→b2→b3...

4. **RTP Control Setup** (Packets #143-152)
   - TCP control messages
   - "RTPC" channel opened with ID 0x517c (Packet #143)
   - Second RTPC with ID 0x517d (Packet #144)
   - Device responds with ID 0xef9e (Packet #146)
   - ACKs exchanged

5. **Camera Start** (Packet #154)
   - TCP command on session 0x5174
   - **Command: 0x11 (VIDEO) / 0x18 (START) / 0x02 (parameter)**
   - Reference to RTPC session: 0x517c
   - ACK (Packet #155)
   - Device confirms (Packet #163)

6. **Camera Configuration** (Packet #198)
   - TCP command on session 0x5174
   - **Command: 0x11 (VIDEO) / 0x14 (CONFIGURE) / 0x32 (50)**
   - Bitrate: 1000 kbps
   - Resolution parameters
   - Reference: 0x517d

7. **UDP Video Stream Starts** (Packet #183+)
   - **UDP packets to port 64100** (same port as TCP control!)
   - Client listens on UDP port 63147
   - Device sends to 192.168.0.126:63147
   - Two video sessions:
     - Session 0x517c: 414 video packets
     - Session 0x517d: 852 video packets (main stream)
   - Average packet: 846 bytes
   - Total: ~1 MB of video data
   - Periodic TCP keep-alives continue

## Video Stream Details (UDP on Port 64100)

### Important Discovery
⚠️ **The video stream uses UDP on port 64100 - the SAME port number as TCP control!**
- TCP connection for control/commands
- UDP packets for video data
- Both protocols use port 64100 simultaneously

### UDP Video Packet Structure

**Small Control Packets** (14-37 bytes):
```
00 06    Magic
06 00    Length (6 bytes payload, LE)
7b 51    Session: 0x517b (LE) - UDPM session
00 00    Flags
6b 12 00 2a    Context/sequence
00 80    Status flags
```

**Video Data Packets** (~180 bytes typical):
```
00 06    Magic
ac 00    Length (172 bytes, LE)
7c 51    Session: 0x517c or 0x517d (LE)
00 00    Flags
80 08 63 a7    Context (appears to be timestamp/counter)
3c a7    Frame counter (increments)
[166 bytes of video data]
```

### Video Session Management

**Session 0x517b** (UDPM control):
- Periodic keepalive packets
- Small 14-18 byte messages
- Sequence numbers increment: 0x2a, 0x2b, 0x2c, 0x2d...

**Session 0x517c** (Video stream 1):
- 414 video packets
- Started at packet #183
- Context starts at 0x63a7... and increments

**Session 0x517d** (Video stream 2 - main):
- 852 video packets  
- Appears to be the primary camera stream
- Larger data volume

### Video Data Format

The video data after the 14-byte header does not show standard H.264 markers (no `00 00 00 01` NAL units). This suggests:
- Custom video codec/compression
- Or encrypted video payload
- Or proprietary framing format

Pattern observed in video data:
```
92 40 25 ff 90 3f [data...]  (typical start)
```
- Byte patterns suggest compressed/encoded video
- Not raw pixel data
- No standard codec signatures detected

## Key Protocol Features

### Session ID Behavior
✓ **Your hypothesis confirmed**: Session ID in bytes 0x04-0x05 (little endian)
- Increments when new logical channels open
- 0x5174 for main device channel
- 0x0000 for control/setup messages

### Message Context (Bytes 0x08-0x0b)
**For Session 0x0000 (Control)**:
- Format: `cd ab [type] 00`
- Type 0x01 = Request
- Type 0x02 = Response

**For Session 0x5174 (Device)**:
- Bit patterns indicate message direction/type
- `c0 18 4e f3` = Client registration
- `00 18 ce f3` = Device response
- `40 18 4e f3` = Client command
- `40 18 ce f3` = Device command response

### Sequence Numbers
- Two-byte counter at offset 0x0c-0x0d
- Increments per message: b1, b2, b3...
- Separate sequences for client and device

## Command Reference

### Command Class 0x11 (VIDEO)

**Subcommand 0x18 (START_STREAM)**
- Parameter: Camera/mode ID
- Requires RTPC session reference
- Returns ACK then confirmation

**Subcommand 0x14 (CONFIGURE)**  
- Parameter: Quality/mode (50 seen)
- Bitrate in kbps (1000 seen)
- Resolution parameters
- Format flags

## Security Analysis

⚠️ **Vulnerabilities:**
- No authentication on port 64100
- Session IDs are predictable/sequential
- Device identifiers transmitted in clear
- Control protocol not encrypted (video is on port 7000)

## Implementation Notes

To replicate camera activation:

1. **TCP connect** to 192.168.0.66:64100
2. Choose session ID (e.g., 0x5174) in **little endian**
3. Send device registration (session 0x5174)
4. Send UDPM setup (session 0x0000, type 0x01)
5. Wait for ACKs
6. Send RTPC setup (session 0x0000, type 0x01) - note the session ID returned
7. Send camera start:
   - Session 0x5174
   - Command 0x11, subcommand 0x18, parameter 0x02
   - Include RTPC reference ID
8. Send configuration if needed
9. **Start UDP listener** on an ephemeral port (e.g., 63147)
10. Video arrives via **UDP to port 64100** from device
11. Maintain TCP keepalives

### Critical Implementation Detail
**Port 64100 is used for BOTH TCP and UDP simultaneously**:
- TCP connection carries control messages
- UDP packets on same port carry video data
- This is valid - different protocols can use the same port number
- Your firewall must allow both TCP and UDP on port 64100

All multi-byte values are **little endian** except the magic bytes 0x0006.

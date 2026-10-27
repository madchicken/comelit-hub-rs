# viper-client

Client for the Comelit ViP (Viper) protocol spoken by the ICONA bridge / HUB on
port 64100: user activation, configuration, open door and video stream from an
external unit.

The protocol was reverse engineered from the official Android app
(`com.comelit.bigapp` 7.3.5): all the ViP logic lives in the native library
`libvipcomelit.so`, whose symbols (`CallFsm`, `csp_send_*`, `viper_tunnel_*`,
`ctp_*`) are still present. `capture.txt` is a TCP capture of the app watching
the external unit; the unit tests in `src/video/` check the encoder against it
byte by byte.

## Usage

```bash
# ICONA_IP / ICONA_TOKEN can also come from viper-client/.env
cargo run -p viper-client -- info
cargo run -p viper-client -- open-door --door-name "Portone"
cargo run -p viper-client -- start-video --target "Portone" --output doorbell --duration 30 [--audio]
ffplay doorbell.h264
ffplay -f alaw -ar 8000 -ac 1 doorbell-audio.alaw
```

`--target` is an entrance or door name from the address books printed by `info`.

## Tunnel framing

TCP and UDP (both port 64100) carry the same frames:

```text
[flags & 3] [06] [body length u16 LE] [channel id u16 LE] [00 00] [body]
```

Channel ids are chosen by whoever opens the channel (the device uses ids with
the top bit set). Channel 0 is the control channel:

| Message | Body |
|---|---|
| open | `cd ab 01 00 07 00 00 00 <name 4> <id> <transport> [00 <len u16> 00 00 <data>]` |
| open response | `cd ab 02 00 04 00 00 00 <id> <status u16> [<len u16> 00 00 <data>]` |
| close | `ef 01 03 00 02 00 00 00 <id>` |

`transport` is 1 only for RTPC: its frames go over UDP once the UDPM lane is
open, otherwise they stay on TCP. CTPP is opened with our logical address as
data (`COMHUB012\0`, i.e. apt address + subaddress, 10 bytes).

## UDPM lane

The UDPM open response carries a 2 byte token. Pings travel over UDP on the
UDPM channel:

```text
[token (2)] [is_reply] [seq] [lane_open] [80]
```

The client pings every 300 ms; the device answers `[token] 01 <seq> 01 80` and
from then on the lane is open (keepalive every ~5 s with `lane_open = 1`).
Device pings must be answered with `is_reply = 1`. The device sends media to
the UDP source address of the pings.

## CTP (on the CTPP channel)

```text
[flags] [18] [conn id (2)] [tx seq] [rx seq] [payload len u16 BE]
[payload, zero padded to 4] [ff ff ff ff] [src logaddr (10)] [dst logaddr (10)]
```

* flags: `0x80` opens the connection (first packet `0xc0`), `0x40` data
  (needs an ack), `0x00` pure ack;
* the device replies with the connection id top bit set (`4e f3` → `ce f3`);
* stop-and-wait: a data packet with `tx = T` is acknowledged by a packet with
  `rx = T + 1`; retransmit otherwise. The client proposes the device initial
  sequence in the `rx` of its first packet.

## CSP messages (CTP payload, little endian fields)

| Type | Name | Layout |
|---|---|---|
| `00 01` | START | `src(10) dst(10) 01 00 <call id u32> src(10) 'I' 'I'` |
| `00 03` | CAPAB_REPORT | `<unit type> 00 <capabilities u32>` (bit `0x08` = video) |
| `00 0a` | ALERTING | |
| `00 0c` | CONNECT | |
| `00 0e` | RELEASE | `<cause>` |
| `00 11` | MEDIAREQ (audio) | `18 02 00000000 <rtpc id>` |
| `00 11` | MEDIAREQ (video) | `14 32 00000000 <rtpc id> <max rtp payload u16> <bitrate u32> <xmax> <ymax> <xpref> <ypref> <fps> 00` |
| `00 12` | MEDIAOFFER | |
| `00 70` | VOICESTATUS | `<logaddr (10)> <on> 00` (`CallFsm::start_fonica` on answer) |

## Watching an external unit

Sequence of the app capture (implemented in `src/video/session.rs`):

```text
C→S  CTRL open CTPP (COMHUB012) / open UDPM          S→C open responses (UDPM token 6b12)
C→S  CTP START  COMHUB012 → 00000100                  S→C ack
C→S  CTP CAPAB_REPORT 'I' 0x27                        S→C ack
S→C  CTP CAPAB_REPORT 'P' 0x1b, CONNECT               C→S acks
C↔S  UDPM pings over UDP until the lane is open
C→S  CTRL open RTPC 517c, 517d                        S→C open responses
S→C  CTRL open RTPC ef9e (to receive our audio)       C→S open response
C→S  CTP MEDIAREQ audio → 517c, MEDIAREQ video → 517d
S→C  UDP: 00 06 <len> 7d 51 00 00 + RTP (H.264, PT 99) / 7c 51 + RTP (PCMA, PT 8)
C→S  CTP RELEASE, CTRL close channels
```

## Long lived unit

`ViperClient::into_unit` turns an authorized connection into a
[`ViperUnit`](src/video/unit.rs), the building block for the HomeKit bridge:

* it keeps CTPP and UDPM open and registers as a subunit of the apartment,
  renewing the registration at 90% of its lifetime (10 min);
* `UnitEvent::Ringing` / `UnitEvent::RingEnded` report incoming calls; our leg
  is answered with CAPAB_REPORT + ALERTING and released after the ring
  timeout;
* `answer` takes the ringing call like the app (`CallFsm::st_in_alerting` on
  event 0x1000): audio MEDIAREQ, CONNECT (`00 0c`), video MEDIAREQ. The
  external unit sends its audio only on an answered call, and the other units
  stop ringing. `hang_up` releases it; an answered call has no ring timeout;
  the app also sends VOICESTATUS on before the audio MEDIAREQ;
* the external unit asks our audio with its own MEDIAREQ `18` (stopped by
  `98`) naming the RTPC channel it opened. `send_voice` sends PCMA there as
  plain RTP, payload type 8, like `Rtp::rtp_tx_write`, over the UDP lane;
* `start_media` returns the RTP packets of the external unit: from the ringing
  call, or by calling the configured entrance when nobody rings. If someone
  rings while the entrance is being watched, the media moves to the incoming
  call; when the incoming call ends while media is still wanted, the unit goes
  back to watching the entrance.

The CLI commands are thin wrappers recording a unit's media to files.

RTP payload types used by the engine: 99 H.264 (RFC 6184, FU-A/STAP-A),
104 MPEG-4 part 2, 26 MJPEG (not recorded), 8 PCMA, 0 PCMU.

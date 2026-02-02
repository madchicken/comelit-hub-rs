use anyhow::{Context, Result};
use bon::Builder;
use bytes::Bytes;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use tokio::net::UdpSocket;
use tokio::signal;
use tracing::{debug, error, info, warn};

use crate::audio_video::audio_assembler::AudioAssembler;

/// RTP Audio/Video capture tool for custom protocol
#[derive(Builder, Debug)]
pub(crate) struct Args {
    /// UDP port to listen on (local port when binding, remote port when connecting)
    port: u16,

    /// Bind address (use 0.0.0.0 to listen on all interfaces)
    bind: String,

    /// Remote address to connect to (e.g., "192.168.1.100"). When set, connects to remote:port instead of binding locally.
    remote: Option<String>,

    /// Output video file path
    video_output: PathBuf,

    /// Output audio file path
    audio_output: PathBuf,

    /// Disable audio capture (video only)
    no_audio: bool,

    /// Disable video capture (audio only)
    no_video: bool,

    /// Maximum packets to capture (0 = unlimited)
    max_packets: usize,
}

/// RTP packet information
#[derive(Debug, Clone)]
struct RtpPacket {
    version: u8,
    marker: bool,
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    payload: Bytes,
}

/// Type of media stream
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamType {
    Video,
    Audio,
    Unknown,
}

/// Detected stream information
#[derive(Debug, Clone)]
struct StreamInfo {
    ssrc: u32,
    stream_type: StreamType,
    payload_type: u8,
    packet_count: usize,
}

/// H.264 NAL unit fragment info
#[derive(Debug)]
struct H264Fragment {
    is_start: bool,
    is_end: bool,
    nal_type: u8,
    data: Bytes,
}

/// Frame assembler for H.264 NAL units
struct VideoAssembler {
    current_frame: Vec<Bytes>,
    frame_count: usize,
    output_file: File,
}

impl VideoAssembler {
    fn new(output_path: &PathBuf) -> Result<Self> {
        let output_file = File::create(output_path).context(format!(
            "Failed to create video output file: {:?}",
            output_path
        ))?;

        Ok(Self {
            current_frame: Vec::new(),
            frame_count: 0,
            output_file,
        })
    }

    fn process_fragment(&mut self, fragment: H264Fragment) -> Result<()> {
        if fragment.is_start {
            // Start of new NAL unit - write previous frame if exists
            if !self.current_frame.is_empty() {
                self.write_frame()?;
            }
            self.current_frame.clear();
            self.current_frame.push(fragment.data);
        } else {
            // Continuation of current NAL unit
            self.current_frame.push(fragment.data);
        }

        if fragment.is_end && !self.current_frame.is_empty() {
            self.write_frame()?;
            self.current_frame.clear();
        }

        Ok(())
    }

    fn write_frame(&mut self) -> Result<()> {
        // Write H.264 start code (00 00 00 01)
        self.output_file.write_all(&[0x00, 0x00, 0x00, 0x01])?;

        // Write NAL unit data
        for chunk in &self.current_frame {
            self.output_file.write_all(chunk)?;
        }

        self.frame_count += 1;

        if self.frame_count.is_multiple_of(100) {
            info!("Written {} video frames", self.frame_count);
        }

        Ok(())
    }

    fn finalize(&mut self) -> Result<usize> {
        // Write any remaining frame
        if !self.current_frame.is_empty() {
            self.write_frame()?;
        }

        self.output_file.flush()?;
        info!("Total video frames written: {}", self.frame_count);
        Ok(self.frame_count)
    }
}

/// Parse custom RTP packet with 00 06 protocol wrapper
fn parse_custom_rtp(data: &[u8]) -> Option<RtpPacket> {
    if data.len() < 20 {
        return None;
    }

    // Check for custom protocol header (00 06)
    if data[0] != 0x00 || data[1] != 0x06 {
        warn!(
            "Not a custom RTP packet (header: {:02x} {:02x})",
            data[0], data[1]
        );
        return None;
    }

    // Skip custom header (8 bytes: 00 06 XX XX SS SS 00 00)
    let rtp_start = 8;

    if data.len() < rtp_start + 12 {
        return None;
    }

    let rtp_data = &data[rtp_start..];

    // Parse RTP header
    let version = (rtp_data[0] >> 6) & 0x03;
    let padding = (rtp_data[0] >> 5) & 0x01;
    let extension = (rtp_data[0] >> 4) & 0x01;
    let csrc_count = rtp_data[0] & 0x0F;

    let marker = ((rtp_data[1] >> 7) & 0x01) != 0;
    let payload_type = rtp_data[1] & 0x7F;

    let sequence = u16::from_be_bytes([rtp_data[2], rtp_data[3]]);
    let timestamp = u32::from_be_bytes([rtp_data[4], rtp_data[5], rtp_data[6], rtp_data[7]]);
    let ssrc = u32::from_be_bytes([rtp_data[8], rtp_data[9], rtp_data[10], rtp_data[11]]);

    // Calculate header length (12 bytes + CSRC)
    let header_len = 12 + (csrc_count as usize * 4);

    if rtp_data.len() < header_len {
        return None;
    }

    // Extract payload
    let payload = Bytes::copy_from_slice(&rtp_data[header_len..]);

    debug!(
        "RTP: seq={}, ts={}, ssrc=0x{:08x}, marker={}, payload_len={}",
        sequence,
        timestamp,
        ssrc,
        marker,
        payload.len()
    );

    Some(RtpPacket {
        version,
        marker,
        payload_type,
        sequence,
        timestamp,
        ssrc,
        payload,
    })
}

/// Detect stream type based on payload type and payload inspection
fn detect_stream_type(payload_type: u8, payload: &[u8]) -> StreamType {
    // Standard audio payload types
    match payload_type {
        0 | 8 | 9 | 10 | 11 => return StreamType::Audio, // G.711, G.722, L16
        _ => {}
    }

    // For dynamic payload types (96-127), inspect the payload
    if payload_type >= 96 && payload.len() >= 2 {
        let first_byte = payload[0];

        // Check for H.264 video (FU-A or NAL units)
        let nal_type = first_byte & 0x1F;
        if nal_type == 28 || (nal_type >= 1 && nal_type <= 5) {
            return StreamType::Video;
        }

        // Check for AAC audio (ADTS header starts with 0xFFF)
        if first_byte == 0xFF && payload.len() > 1 && (payload[1] & 0xF0) == 0xF0 {
            return StreamType::Audio;
        }

        // Small packets (< 500 bytes) are likely audio (Opus, G.711, etc.)
        if payload.len() < 500 {
            return StreamType::Audio;
        }
    }

    StreamType::Unknown
}

/// Process H.264 RTP payload (handles FU-A fragmentation)
fn process_h264_payload(payload: &Bytes) -> Option<H264Fragment> {
    if payload.len() < 2 {
        return None;
    }

    let fu_indicator = payload[0];
    let nal_type = fu_indicator & 0x1F;

    // Check if this is FU-A (fragmented)
    if nal_type == 28 {
        let fu_header = payload[1];
        let start_bit = ((fu_header >> 7) & 0x01) != 0;
        let end_bit = ((fu_header >> 6) & 0x01) != 0;
        let fragment_nal_type = fu_header & 0x1F;

        if start_bit {
            // Start of fragment - reconstruct NAL header
            let nal_header = (fu_indicator & 0xE0) | fragment_nal_type;
            let mut data = Vec::with_capacity(payload.len() - 1);
            data.push(nal_header);
            data.extend_from_slice(&payload[2..]);

            debug!(
                "FU-A START: nal_type={}, len={}",
                fragment_nal_type,
                data.len()
            );

            Some(H264Fragment {
                is_start: true,
                is_end: end_bit,
                nal_type: fragment_nal_type,
                data: Bytes::from(data),
            })
        } else {
            // Continuation or end of fragment
            let data = Bytes::copy_from_slice(&payload[2..]);

            if end_bit {
                debug!("FU-A END: len={}", data.len());
            } else {
                debug!("FU-A CONT: len={}", data.len());
            }

            Some(H264Fragment {
                is_start: false,
                is_end: end_bit,
                nal_type: fragment_nal_type,
                data,
            })
        }
    } else {
        // Single NAL unit (not fragmented)
        debug!("Single NAL: type={}, len={}", nal_type, payload.len());

        Some(H264Fragment {
            is_start: true,
            is_end: true,
            nal_type,
            data: payload.clone(),
        })
    }
}

async fn capture_rtp(args: Args) -> Result<()> {
    let socket = if let Some(ref remote) = args.remote {
        // Connect to remote address - bind to local address first, then connect
        let bind_addr = format!("{}:0", args.bind); // Use port 0 to let OS assign a local port
        let socket = UdpSocket::bind(&bind_addr)
            .await
            .context(format!("Failed to bind to {}", bind_addr))?;

        let remote_addr = format!("{}:{}", remote, args.port);
        socket
            .connect(&remote_addr)
            .await
            .context(format!("Failed to connect to {}", remote_addr))?;

        info!("Connected to remote UDP {}", remote_addr);
        socket
    } else {
        // Bind locally and listen for incoming packets
        let bind_addr = format!("{}:{}", args.bind, args.port);
        let socket = UdpSocket::bind(&bind_addr)
            .await
            .context(format!("Failed to bind to {}", bind_addr))?;

        info!("Listening on UDP {}", bind_addr);
        socket
    };

    if !args.no_video {
        info!("Video output: {:?}", args.video_output);
    }
    if !args.no_audio {
        info!("Audio output: {:?}", args.audio_output);
    }

    info!("Press Ctrl+C to stop capture");

    // Create assemblers
    let mut video_assembler = if !args.no_video {
        Some(VideoAssembler::new(&args.video_output)?)
    } else {
        None
    };

    let mut audio_assembler = if !args.no_audio {
        Some(AudioAssembler::new(&args.audio_output)?)
    } else {
        None
    };

    // Statistics
    let mut total_packets = 0;
    let mut rtp_packets = 0;
    let mut streams: HashMap<u32, StreamInfo> = HashMap::new();
    let mut video_ssrc: Option<u32> = None;
    let mut audio_ssrc: Option<u32> = None;

    let mut buf = vec![0u8; 2048];

    loop {
        tokio::select! {
            // Handle UDP packets
            result = async {
                if args.remote.is_some() {
                    // When connected, use recv() which only receives from the connected address
                    socket.recv(&mut buf).await.map(|len| (len, "connected remote".to_string()))
                } else {
                    // When not connected, use recv_from() to get packets from any source
                    socket.recv_from(&mut buf).await.map(|(len, addr)| (len, addr.to_string()))
                }
            } => {
                match result {
                    Ok((len, addr)) => {
                        total_packets += 1;

                        debug!("Received {} bytes from {}", len, addr);

                        // Parse RTP packet
                        if let Some(rtp) = parse_custom_rtp(&buf[..len]) {
                            rtp_packets += 1;

                            // Detect stream type on first packet from this SSRC
                            if let std::collections::hash_map::Entry::Vacant(e) = streams.entry(rtp.ssrc) {
                                let stream_type = detect_stream_type(rtp.payload_type, &rtp.payload);

                                let stream_info = StreamInfo {
                                    ssrc: rtp.ssrc,
                                    stream_type,
                                    payload_type: rtp.payload_type,
                                    packet_count: 1,
                                };

                                info!("New stream detected: SSRC=0x{:08x}, Type={:?}, PT={}",
                                      rtp.ssrc, stream_type, rtp.payload_type);

                                // Assign to video or audio slot if available
                                match stream_type {
                                    StreamType::Video if video_ssrc.is_none() && !args.no_video => {
                                        video_ssrc = Some(rtp.ssrc);
                                        info!("  → Assigned as VIDEO stream");
                                    }
                                    StreamType::Audio if audio_ssrc.is_none() && !args.no_audio => {
                                        audio_ssrc = Some(rtp.ssrc);
                                        info!("  → Assigned as AUDIO stream");
                                    }
                                    _ => {
                                        debug!("  → Ignoring (duplicate or disabled)");
                                    }
                                }

                                e.insert(stream_info);
                            } else {
                                // Update packet count
                                if let Some(info) = streams.get_mut(&rtp.ssrc) {
                                    info.packet_count += 1;
                                }
                            }

                            // Process video stream
                            if Some(rtp.ssrc) == video_ssrc
                                && let Some(ref mut assembler) = video_assembler
                                && let Some(fragment) = process_h264_payload(&rtp.payload)
                                && let Err(e) = assembler.process_fragment(fragment) {
                                error!("Failed to process video fragment: {}", e);
                            }

                            // Process audio stream
                            if Some(rtp.ssrc) == audio_ssrc
                                && let Some(ref mut assembler) = audio_assembler
                                && let Err(e) = assembler.process_packet(&rtp.payload) {
                                error!("Failed to process audio packet: {}", e);
                            }

                            // Check if we've reached max packets
                            if args.max_packets > 0 && rtp_packets >= args.max_packets {
                                info!("Reached max packets limit ({})", args.max_packets);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!("Socket error: {}", e);
                    }
                }
            }

            // Handle Ctrl+C
            _ = signal::ctrl_c() => {
                info!("\nReceived Ctrl+C, stopping capture...");
                break;
            }
        }
    }

    // Finalize and print statistics
    info!("\n=== Finalizing Streams ===");

    let mut video_frames = 0;
    let mut audio_packets = 0;

    if let Some(ref mut assembler) = video_assembler {
        video_frames = assembler.finalize()?;
    }

    if let Some(ref mut assembler) = audio_assembler {
        audio_packets = assembler.finalize()?;
    }

    info!("\n=== Capture Statistics ===");
    info!("Total packets received: {}", total_packets);
    info!("RTP packets parsed: {}", rtp_packets);
    info!("Streams detected: {}", streams.len());

    for (ssrc, info) in &streams {
        info!(
            "  SSRC 0x{:08x}: {:?}, PT={}, {} packets",
            ssrc, info.stream_type, info.payload_type, info.packet_count
        );
    }

    if video_frames > 0 {
        info!(
            "\nVideo output: {:?} ({} frames)",
            args.video_output, video_frames
        );
        info!("  To play: ffplay {:?}", args.video_output);
    }

    if audio_packets > 0 {
        info!(
            "\nAudio output: {:?} ({} packets)",
            args.audio_output, audio_packets
        );

        // Provide conversion hint based on common payload types
        if let Some(ssrc) = audio_ssrc
            && let Some(info) = streams.get(&ssrc)
        {
            match info.payload_type {
                0 => info!(
                    "  To convert: ffmpeg -f mulaw -ar 8000 -ac 1 -i {:?} audio.wav",
                    args.audio_output
                ),
                8 => info!(
                    "  To convert: ffmpeg -f alaw -ar 8000 -ac 1 -i {:?} audio.wav",
                    args.audio_output
                ),
                _ => info!("  Audio codec: PT={}", info.payload_type),
            }
        }
    }

    if video_frames > 0 && audio_packets > 0 {
        info!("\nTo combine audio and video:");
        info!(
            "  ffmpeg -i {:?} -f mulaw -ar 8000 -ac 1 -i {:?} -c:v copy -c:a aac output.mp4",
            args.video_output, args.audio_output
        );
    }

    Ok(())
}

pub(crate) async fn read_av_stream(args: Args) -> Result<()> {
    info!(
        "RTP Audio/Video Capture Tool v{}",
        env!("CARGO_PKG_VERSION")
    );

    capture_rtp(args).await
}

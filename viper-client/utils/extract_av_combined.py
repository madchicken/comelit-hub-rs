#!/usr/bin/env python3
"""
Extract both audio and video from custom RTP protocol and mux into single file.
Outputs a container format (MP4/MKV) that FFmpeg can play directly.
"""

import struct
import sys
import subprocess
import tempfile
import os
from collections import defaultdict

try:
    from scapy.all import rdpcap, UDP
except ImportError:
    print("Error: scapy is required. Install with: pip install scapy")
    sys.exit(1)


def parse_rtp_packet(udp_payload):
    """Parse custom RTP packet structure."""
    if len(udp_payload) < 20:
        return None
    
    # Skip custom protocol header (8 bytes)
    rtp_start = 8
    
    if len(udp_payload) < rtp_start + 12:
        return None
    
    rtp_data = udp_payload[rtp_start:]
    
    # Parse RTP header
    marker = (rtp_data[1] >> 7) & 0x01
    payload_type = rtp_data[1] & 0x7F
    sequence = struct.unpack('>H', rtp_data[2:4])[0]
    timestamp = struct.unpack('>I', rtp_data[4:8])[0]
    ssrc = struct.unpack('>I', rtp_data[8:12])[0]
    
    # Get payload (after 12-byte RTP header)
    payload = rtp_data[12:]
    
    return {
        'marker': marker,
        'payload_type': payload_type,
        'sequence': sequence,
        'timestamp': timestamp,
        'ssrc': ssrc,
        'payload': payload
    }


def detect_stream_type(payload_type, packets):
    """Detect if stream is video, audio, or control."""
    if not packets:
        return "unknown", None
    
    # Standard audio payload types
    audio_codecs = {
        0: "pcmu",
        8: "pcma",
        9: "g722",
        10: "l16",
        11: "l16",
    }
    
    if payload_type in audio_codecs:
        return "audio", audio_codecs[payload_type]
    
    # For dynamic payload types (96-127), inspect payload
    if payload_type >= 96:
        avg_size = sum(len(p['payload']) for p in packets[:20]) // min(20, len(packets))
        
        # Check first few packets
        for pkt in packets[:10]:
            if len(pkt['payload']) > 2:
                first_byte = pkt['payload'][0]
                
                # H.264 video detection
                nal_type = first_byte & 0x1F
                if nal_type == 28 or (nal_type >= 1 and nal_type <= 5):
                    return "video", "h264"
                
                # AAC audio detection
                if first_byte == 0xFF and (pkt['payload'][1] & 0xF0) == 0xF0:
                    return "audio", "aac"
        
        # Small packets likely audio
        if avg_size < 500:
            return "audio", "opus"
    
    # Very small packets = control data
    if packets:
        avg_size = sum(len(p['payload']) for p in packets) // len(packets)
        if avg_size < 100:
            return "control", None
    
    return "unknown", None


def extract_h264_video(packets, output_file):
    """Extract H.264 video stream."""
    print(f"  Extracting H.264 video...")
    
    current_frame = []
    frame_count = 0
    
    with open(output_file, 'wb') as f:
        for pkt in packets:
            payload = pkt['payload']
            
            if len(payload) < 2:
                continue
            
            fu_indicator = payload[0]
            nal_type = fu_indicator & 0x1F
            
            # Handle FU-A fragmentation
            if nal_type == 28:
                fu_header = payload[1]
                start_bit = (fu_header >> 7) & 0x01
                end_bit = (fu_header >> 6) & 0x01
                fragment_nal_type = fu_header & 0x1F
                
                if start_bit:
                    # Write previous frame
                    if current_frame:
                        f.write(bytes([0, 0, 0, 1]) + b''.join(current_frame))
                        frame_count += 1
                    
                    # Start new frame
                    nal_header = (fu_indicator & 0xE0) | fragment_nal_type
                    current_frame = [bytes([nal_header]) + payload[2:]]
                else:
                    current_frame.append(payload[2:])
                
                if end_bit and current_frame:
                    f.write(bytes([0, 0, 0, 1]) + b''.join(current_frame))
                    frame_count += 1
                    current_frame = []
            else:
                # Single NAL unit
                f.write(bytes([0, 0, 0, 1]) + payload)
                frame_count += 1
        
        # Write final frame
        if current_frame:
            f.write(bytes([0, 0, 0, 1]) + b''.join(current_frame))
            frame_count += 1
    
    print(f"  ✓ Extracted {frame_count} video frames")
    return frame_count > 0


def extract_audio(packets, output_file, codec):
    """Extract audio stream."""
    print(f"  Extracting {codec} audio...")
    
    with open(output_file, 'wb') as f:
        for pkt in packets:
            f.write(pkt['payload'])
    
    sample_count = sum(len(p['payload']) for p in packets)
    print(f"  ✓ Extracted {sample_count} bytes of audio")
    return sample_count > 0


def mux_audio_video(video_file, audio_file, audio_codec, output_file):
    """Mux audio and video using FFmpeg."""
    print(f"\nMuxing audio and video into: {output_file}")
    
    # Build FFmpeg command
    cmd = ['ffmpeg', '-y']  # -y to overwrite
    
    # Video input
    cmd.extend(['-i', video_file])
    
    # Audio input with format specification
    if audio_codec == "pcmu":
        cmd.extend(['-f', 'mulaw', '-ar', '8000', '-ac', '1', '-i', audio_file])
    elif audio_codec == "pcma":
        cmd.extend(['-f', 'alaw', '-ar', '8000', '-ac', '1', '-i', audio_file])
    elif audio_codec == "aac":
        cmd.extend(['-i', audio_file])
    elif audio_codec == "opus":
        cmd.extend(['-f', 'opus', '-i', audio_file])
    else:
        print(f"  ⚠️  Unknown audio codec: {audio_codec}, trying raw...")
        cmd.extend(['-i', audio_file])
    
    # Output options
    cmd.extend([
        '-c:v', 'copy',  # Copy video without re-encoding
        '-c:a', 'aac',   # Encode audio to AAC (compatible)
        '-strict', 'experimental',
        output_file
    ])
    
    print(f"  Running: {' '.join(cmd)}")
    
    try:
        result = subprocess.run(cmd, capture_output=True, text=True)
        
        if result.returncode == 0:
            print(f"  ✓ Successfully created: {output_file}")
            return True
        else:
            print(f"  ✗ FFmpeg error:")
            print(result.stderr)
            return False
    except FileNotFoundError:
        print("  ✗ FFmpeg not found! Please install FFmpeg.")
        return False


def extract_and_mux(pcap_file, output_file):
    """Extract audio and video from pcap and mux into single file."""
    print(f"Reading pcap file: {pcap_file}")
    
    try:
        packets = rdpcap(pcap_file)
    except Exception as e:
        print(f"Error reading pcap: {e}")
        return False
    
    print(f"Found {len(packets)} packets\n")
    
    # Group packets by SSRC
    streams = defaultdict(list)
    
    for pkt in packets:
        if UDP in pkt:
            udp_payload = bytes(pkt[UDP].payload)
            
            if len(udp_payload) < 8:
                continue
            
            if udp_payload[0] == 0x00 and udp_payload[1] == 0x06:
                rtp_info = parse_rtp_packet(udp_payload)
                
                if rtp_info:
                    streams[rtp_info['ssrc']].append(rtp_info)
    
    print(f"Found {len(streams)} RTP stream(s)\n")
    
    # Identify streams
    video_stream = None
    audio_stream = None
    video_codec = None
    audio_codec = None
    
    for ssrc, stream_packets in streams.items():
        stream_packets.sort(key=lambda x: x['sequence'])
        
        stream_type, codec = detect_stream_type(stream_packets[0]['payload_type'], stream_packets)
        
        avg_size = sum(len(p['payload']) for p in stream_packets) // len(stream_packets)
        
        print(f"Stream SSRC 0x{ssrc:08x}:")
        print(f"  Type: {stream_type}")
        print(f"  Codec: {codec}")
        print(f"  Packets: {len(stream_packets)}")
        print(f"  Avg size: {avg_size} bytes")
        
        if stream_type == "video" and video_stream is None:
            video_stream = stream_packets
            video_codec = codec
            print(f"  → Selected as VIDEO stream")
        elif stream_type == "audio" and audio_stream is None:
            audio_stream = stream_packets
            audio_codec = codec
            print(f"  → Selected as AUDIO stream")
        else:
            print(f"  → Skipping (control or duplicate)")
        
        print()
    
    # Check what we found
    if video_stream is None and audio_stream is None:
        print("✗ No video or audio streams found!")
        return False
    
    # Create temporary files
    temp_dir = tempfile.mkdtemp()
    video_file = os.path.join(temp_dir, "video.h264") if video_stream else None
    audio_file = os.path.join(temp_dir, f"audio.{audio_codec}") if audio_stream else None
    
    try:
        # Extract video
        if video_stream:
            print("="*60)
            print("VIDEO EXTRACTION")
            print("="*60)
            if not extract_h264_video(video_stream, video_file):
                print("✗ Video extraction failed!")
                return False
        
        # Extract audio
        if audio_stream:
            print("\n" + "="*60)
            print("AUDIO EXTRACTION")
            print("="*60)
            if not extract_audio(audio_stream, audio_file, audio_codec):
                print("✗ Audio extraction failed!")
                return False
        
        # Mux or copy
        print("\n" + "="*60)
        print("FINAL OUTPUT")
        print("="*60)
        
        if video_stream and audio_stream:
            # Both streams - mux together
            success = mux_audio_video(video_file, audio_file, audio_codec, output_file)
        elif video_stream:
            # Video only - convert to MP4
            print(f"Video-only output: {output_file}")
            cmd = ['ffmpeg', '-y', '-i', video_file, '-c:v', 'copy', output_file]
            result = subprocess.run(cmd, capture_output=True)
            success = result.returncode == 0
        elif audio_stream:
            # Audio only - convert to suitable format
            print(f"Audio-only output: {output_file}")
            if audio_codec in ["pcmu", "pcma"]:
                fmt = "mulaw" if audio_codec == "pcmu" else "alaw"
                cmd = ['ffmpeg', '-y', '-f', fmt, '-ar', '8000', '-ac', '1', 
                       '-i', audio_file, '-c:a', 'aac', output_file]
            else:
                cmd = ['ffmpeg', '-y', '-i', audio_file, '-c:a', 'aac', output_file]
            result = subprocess.run(cmd, capture_output=True)
            success = result.returncode == 0
        
        if success:
            file_size = os.path.getsize(output_file)
            print(f"\n✓ Successfully created: {output_file}")
            print(f"  File size: {file_size / 1024 / 1024:.2f} MB")
            print(f"\nYou can now play it with:")
            print(f"  ffplay {output_file}")
            print(f"  vlc {output_file}")
            return True
        else:
            print("\n✗ Muxing failed!")
            return False
            
    finally:
        # Cleanup temp files
        import shutil
        if os.path.exists(temp_dir):
            shutil.rmtree(temp_dir)


def main():
    if len(sys.argv) < 2:
        print("Usage: python extract_av_combined.py <input.pcap> [output.mp4]")
        print("\nExtracts both audio and video from pcap and muxes into single file")
        print("\nExample:")
        print("  python extract_av_combined.py capture.pcap output.mp4")
        print("  python extract_av_combined.py capture.pcap recording.mkv")
        sys.exit(1)
    
    pcap_file = sys.argv[1]
    output_file = sys.argv[2] if len(sys.argv) > 2 else "output.mp4"
    
    print("="*60)
    print("RTP Audio/Video Extraction and Muxing Tool")
    print("="*60)
    print()
    
    success = extract_and_mux(pcap_file, output_file)
    
    if success:
        print("\n" + "="*60)
        print("✓ COMPLETE!")
        print("="*60)
    else:
        print("\n" + "="*60)
        print("✗ FAILED")
        print("="*60)
        sys.exit(1)


if __name__ == '__main__':
    main()

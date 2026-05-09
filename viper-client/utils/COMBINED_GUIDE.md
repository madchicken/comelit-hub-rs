# Combined Audio/Video Extraction Guide

## Quick Start

### Single Command - Get Everything!
```bash
python extract_av_combined.py capture.pcap output.mp4
```

That's it! This will:
1. ✅ Extract video (H.264)
2. ✅ Extract audio (G.711/AAC/Opus)
3. ✅ Mux them together into a single MP4 file
4. ✅ Output ready to play!

## Requirements

```bash
pip install scapy
```

Plus FFmpeg must be installed:
```bash
# Ubuntu/Debian
sudo apt install ffmpeg

# macOS
brew install ffmpeg

# Windows
# Download from: https://ffmpeg.org/download.html
```

## Usage Examples

### Basic Usage
```bash
# Extract to MP4 (default)
python extract_av_combined.py recording.pcap

# Specify output file
python extract_av_combined.py recording.pcap my_video.mp4

# Use MKV format (better codec support)
python extract_av_combined.py recording.pcap output.mkv
```

### Example Output
```
============================================================
RTP Audio/Video Extraction and Muxing Tool
============================================================

Reading pcap file: capture.pcap
Found 2847 packets

Found 3 RTP stream(s)

Stream SSRC 0xc0809a0d:
  Type: video
  Codec: h264
  Packets: 1498
  Avg size: 1234 bytes
  → Selected as VIDEO stream

Stream SSRC 0x12345678:
  Type: audio
  Codec: pcmu
  Packets: 856
  Avg size: 160 bytes
  → Selected as AUDIO stream

Stream SSRC 0xabcdef00:
  Type: control
  Codec: None
  Packets: 23
  Avg size: 45 bytes
  → Skipping (control or duplicate)

============================================================
VIDEO EXTRACTION
============================================================
  Extracting H.264 video...
  ✓ Extracted 342 video frames

============================================================
AUDIO EXTRACTION
============================================================
  Extracting pcmu audio...
  ✓ Extracted 136960 bytes of audio

============================================================
FINAL OUTPUT
============================================================
Muxing audio and video into: output.mp4
  Running: ffmpeg -y -i /tmp/.../video.h264 -f mulaw -ar 8000 -ac 1 -i /tmp/.../audio.pcmu -c:v copy -c:a aac -strict experimental output.mp4
  ✓ Successfully created: output.mp4
  File size: 2.34 MB

You can now play it with:
  ffplay output.mp4
  vlc output.mp4

============================================================
✓ COMPLETE!
============================================================
```

## Supported Formats

### Video Codecs
- ✅ H.264 (most common)
- ⚠️ H.265 (requires modification)

### Audio Codecs
- ✅ G.711 μ-law (PCMU) - Payload Type 0
- ✅ G.711 A-law (PCMA) - Payload Type 8
- ✅ AAC - Dynamic payload type
- ⚠️ Opus - Supported but may need tweaking
- ⚠️ G.722, L16 - Raw extraction only

### Output Formats
- ✅ MP4 (recommended, universal compatibility)
- ✅ MKV (better codec support)
- ✅ AVI (legacy compatibility)

## What If I Only Have Video or Audio?

The script handles this automatically:

```bash
# Video-only capture
python extract_av_combined.py video_only.pcap output.mp4
# → Creates video-only MP4

# Audio-only capture
python extract_av_combined.py audio_only.pcap output.mp4
# → Creates audio-only MP4
```

## Comparison with Separate Tools

| Feature | Combined Tool | Separate Tools |
|---------|---------------|----------------|
| **One command** | ✅ Yes | ❌ No (3 commands) |
| **Auto-mux** | ✅ Yes | ❌ Manual FFmpeg |
| **Ready to play** | ✅ Yes | ❌ Needs conversion |
| **File handling** | ✅ Automatic cleanup | ❌ Manual cleanup |
| **Stream detection** | ✅ Automatic | ⚠️ Manual selection |
| **Fine control** | ❌ Limited | ✅ Full control |

### When to Use Combined Tool:
- ✅ Quick extraction for viewing
- ✅ You want audio + video together
- ✅ You want a single playable file
- ✅ You don't need fine-grained control

### When to Use Separate Tools:
- ✅ You only want video or audio
- ✅ You need specific codec settings
- ✅ You want to process streams separately
- ✅ You're debugging stream issues

## Workflow Comparison

### Old Way (Separate Tools):
```bash
# Step 1: Extract video
python extract_h264_from_pcap.py capture.pcap video.h264

# Step 2: Extract audio
python extract_audio_from_pcap.py capture.pcap audio

# Step 3: Convert audio
ffmpeg -f mulaw -ar 8000 -ac 1 -i audio_ssrc_12345678.pcm audio.wav

# Step 4: Combine
ffmpeg -i video.h264 -i audio.wav -c:v copy -c:a aac output.mp4

# Step 5: Cleanup
rm video.h264 audio_ssrc_*.pcm audio.wav
```

### New Way (Combined Tool):
```bash
python extract_av_combined.py capture.pcap output.mp4
```

## Troubleshooting

### "FFmpeg not found"
```bash
# Install FFmpeg
sudo apt install ffmpeg  # Linux
brew install ffmpeg      # macOS
```

### "No video or audio streams found"
- Check if pcap has the custom `00 06` protocol
- Verify UDP traffic on expected ports
- Try separate tools for detailed diagnostics

### Audio/Video out of sync
- This can happen if timestamps aren't aligned
- Try re-encoding instead of copying:
```bash
# Manual re-sync with FFmpeg
ffmpeg -i output.mp4 -c:v libx264 -c:a aac -async 1 output_synced.mp4
```

### Large file size
- Video is copied without re-encoding (fast, preserves quality)
- To reduce size, re-encode:
```bash
ffmpeg -i output.mp4 -c:v libx264 -crf 23 -c:a aac output_smaller.mp4
```

### Only getting one stream
- Script picks first video and first audio stream
- If multiple streams, use separate tools for control
- Check diagnostic output to see what was detected

## Advanced Usage

### Extract from specific port only
Modify pcap first:
```bash
tcpdump -r capture.pcap -w filtered.pcap 'udp port 56270'
python extract_av_combined.py filtered.pcap output.mp4
```

### Change output quality
The script uses `-c:v copy` (no re-encoding). To change quality, modify the script or post-process:
```bash
# Higher quality (larger file)
ffmpeg -i output.mp4 -c:v libx264 -crf 18 -c:a aac high_quality.mp4

# Lower quality (smaller file)
ffmpeg -i output.mp4 -c:v libx264 -crf 28 -c:a aac low_quality.mp4
```

### Extract specific time range
```bash
# Filter pcap first (requires editcap from Wireshark)
editcap -A "2024-01-30 10:00:00" -B "2024-01-30 10:05:00" capture.pcap filtered.pcap
python extract_av_combined.py filtered.pcap output.mp4
```

## Tips

1. **Use MP4 for compatibility** - Works everywhere
2. **Use MKV for flexibility** - Better codec support
3. **Check file size** - If suspiciously small, check for errors
4. **Test playback** - Use VLC (most forgiving) first
5. **Keep original pcap** - Don't delete until verified!

## All Available Tools

You now have 4 extraction tools:

1. **`extract_av_combined.py`** ⭐ **RECOMMENDED** - One-stop solution
2. **`extract_h264_from_pcap.py`** - Video only (detailed control)
3. **`extract_audio_from_pcap.py`** - Audio only (multiple codecs)
4. **`extract_h264_dpkt.py`** - Video only (alternative library)

Choose based on your needs!

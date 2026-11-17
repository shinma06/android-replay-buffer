"""Regenerate the synthetic AAC regression input; FFmpeg is a test-authoring tool only."""
import math
from pathlib import Path
import struct
import subprocess

pcm = b"".join(
    struct.pack("<h", round(16000 * math.sin(2 * math.pi * 1000 * i / 48000)))
    if i % 48000 < 4800 else b"\0\0"
    for i in range(8 * 48000)
)
subprocess.run([
    "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
    "-f", "s16le", "-ar", "48000", "-ac", "1", "-i", "pipe:0",
    "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
    str(Path(__file__).with_name("beeps.m4a")),
], input=pcm, check=True)

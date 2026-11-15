"""Synthetic stereo control; FFmpeg is only used when authoring this checked-in input."""
import math
from pathlib import Path
import struct
import subprocess

rate = 44100
pcm = b"".join(
    struct.pack("<hh", *(
        round(12000 * math.sin(2 * math.pi * hz * i / rate)) if i % rate < rate // 10 else 0
        for hz in (440, 880)
    ))
    for i in range(8 * rate)
)
subprocess.run([
    "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
    "-f", "s16le", "-ar", str(rate), "-ac", "2", "-i", "pipe:0",
    "-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart",
    str(Path(__file__).with_name("stereo-44100.m4a")),
], input=pcm, check=True)

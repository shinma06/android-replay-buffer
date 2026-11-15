"""純合成PNS回帰入力。FFmpegは生成時のみ必要。"""
import math
from pathlib import Path
import struct
import subprocess
import tempfile
import wave

root = Path(__file__).resolve().parent
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / "source.wav"
    with wave.open(str(source), "wb") as output:
        output.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        output.writeframes(b"".join(
            struct.pack("<h", round(12000 * math.sin(2 * math.pi * 1000 * i / 48000)))
            if 1920 <= i < 6720 else b"\0\0" for i in range(12000)))
    for enabled, name in [(1, "pns"), (0, "no-pns")]:
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(source),
                        "-c:a", "aac", "-profile:a", "aac_low", "-b:a", "96k",
                        "-aac_pns", str(enabled), str(root / (name + ".m4a"))], check=True)

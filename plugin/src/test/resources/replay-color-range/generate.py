"""Offline synthetic fixture generation only; tests and product do not need ffmpeg."""
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
COLORS = [(0, 0, 0), (255, 255, 255), (64, 64, 64), (128, 128, 128),
          (192, 192, 192), (255, 0, 0), (0, 255, 0), (0, 0, 255)]
FRAME = b"".join(bytes(color) * 32 for color in COLORS) * 32

bt709 = "--bt709-only" in sys.argv[1:]
for name in (("bt709-limited", "bt709-full") if bt709 else ("limited", "full", "default")):
    full = name.endswith("full")
    matrix = "bt709" if bt709 else "bt601"
    description = "bt709" if bt709 else "smpte170m"
    command = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
               "-f", "rawvideo", "-pixel_format", "rgb24", "-video_size", "256x32",
               "-framerate", "10", "-i", "pipe:0",
               "-vf", "scale=out_color_matrix=" + matrix + ":out_range=" + ("full" if full else "limited"),
               "-pix_fmt", "yuvj420p" if full else "yuv420p",
               "-c:v", "libx264", "-profile:v", "baseline", "-preset", "medium", "-qp", "1",
               "-x264-params", "keyint=4:min-keyint=4:scenecut=0:bframes=0:aud=1:threads=1"]
    if name != "default":
        command += ["-color_range", "pc" if full else "tv", "-colorspace", description,
                    "-color_primaries", description, "-color_trc", description]
    command += ["-f", "h264", str(HERE / (name + ".h264"))]
    subprocess.run(command, input=FRAME * 12, check=True)

    reference = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error",
                                "-i", str(HERE / (name + ".h264")), "-frames:v", "1",
                                "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"],
                               capture_output=True, check=True).stdout
    print(name, [tuple(reference[(16 * 256 + 16 + n * 32) * 3:][:3]) for n in range(8)])

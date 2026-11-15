"""Offline synthetic fixture generation only; tests and product do not need ffmpeg."""
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
COLORS = [(0, 0, 0), (255, 255, 255), (64, 64, 64), (128, 128, 128),
          (192, 192, 192), (255, 0, 0), (0, 255, 0), (0, 0, 255)]
FRAME = b"".join(bytes(color) * 32 for color in COLORS) * 32

for name in ("limited", "full", "default"):
    full = name == "full"
    command = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
               "-f", "rawvideo", "-pixel_format", "rgb24", "-video_size", "256x32",
               "-framerate", "10", "-i", "pipe:0",
               "-vf", "scale=out_color_matrix=bt601:out_range=" + ("full" if full else "limited"),
               "-pix_fmt", "yuvj420p" if full else "yuv420p",
               "-c:v", "libx264", "-profile:v", "baseline", "-preset", "medium", "-qp", "1",
               "-x264-params", "keyint=4:min-keyint=4:scenecut=0:bframes=0:aud=1:threads=1"]
    if name != "default":
        command += ["-color_range", "pc" if full else "tv", "-colorspace", "smpte170m",
                    "-color_primaries", "smpte170m", "-color_trc", "smpte170m"]
    command += ["-f", "h264", str(HERE / (name + ".h264"))]
    subprocess.run(command, input=FRAME * 12, check=True)

    reference = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error",
                                "-i", str(HERE / (name + ".h264")), "-frames:v", "1",
                                "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"],
                               capture_output=True, check=True).stdout
    print(name, [tuple(reference[(16 * 256 + 16 + n * 32) * 3:][:3]) for n in range(8)])

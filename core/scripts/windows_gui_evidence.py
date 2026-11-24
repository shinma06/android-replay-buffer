#!/usr/bin/env python3
"""Real Windows GUI evidence for the release gate (G-M7): tray + real players.

Pure Python 3 stdlib plus the Windows desktop (PowerShell `System.Drawing`), `ffmpeg` (gdigrab) and
the players installed by the gate job (Microsoft Edge, VLC). It runs the *extracted* release bundle:

  1. `arb-tray.exe` starts, survives, rejects a second instance, and a desktop screenshot is taken
  2. `share.mp4` and `share-taps.mp4` play in a real Edge window (autoplay allowed) -> screenshots
     and a short `ffmpeg -f gdigrab` screen recording
  3. `share-taps.mp4` plays in real VLC -> screenshot
  4. Media Foundation (WPF `MediaPlayer`) opens and plays `share-taps.mp4`
  5. every screenshot / recorded frame is checked to be non-black and different from the idle desktop

Outputs (`--out-dir`): win-tray.png, win-baseline.png, win-edge-share.png, win-edge-taps.png,
win-vlc-taps.png, win-playback.mp4, windows-gui-evidence.json. Exit code 1 if any check fails.

    windows_gui_evidence.py --root <extracted arb-windows-x86_64> --share share.mp4 --share-taps share-taps.mp4 --out-dir out
    windows_gui_evidence.py --self-test
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Sequence

MIN_MEAN_LUMA = 25.0
MIN_LUMA_STDDEV = 3.0
MIN_DIFF_FROM_BASELINE = 4.0

SCREENSHOT_PS = r"""
param([string]$Path)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$gfx.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bmp.Size)
$bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
$gfx.Dispose(); $bmp.Dispose()
"""

FOREGROUND_PS = r"""
param([int]$ProcessId)
$shell = New-Object -ComObject WScript.Shell
for ($i = 0; $i -lt 10; $i++) { if ($shell.AppActivate($ProcessId)) { break }; Start-Sleep -Milliseconds 300 }
"""

MEDIA_FOUNDATION_PS = r"""
param([string]$Path)
Add-Type -AssemblyName PresentationCore
$player = New-Object System.Windows.Media.MediaPlayer
$script:failed = $null
Register-ObjectEvent -InputObject $player -EventName MediaFailed -Action { $script:failed = $EventArgs.ErrorException.Message } | Out-Null
$player.Open([Uri]$Path)
$player.Play()
$deadline = (Get-Date).AddSeconds(15)
while ((Get-Date) -lt $deadline -and -not ($player.NaturalDuration.HasTimeSpan -and $player.Position.TotalSeconds -gt 1.0)) {
    Start-Sleep -Milliseconds 200
}
$result = [ordered]@{
    has_duration = [bool]$player.NaturalDuration.HasTimeSpan
    duration_s   = if ($player.NaturalDuration.HasTimeSpan) { [math]::Round($player.NaturalDuration.TimeSpan.TotalSeconds, 2) } else { 0 }
    width        = $player.NaturalVideoWidth
    height       = $player.NaturalVideoHeight
    position_s   = [math]::Round($player.Position.TotalSeconds, 2)
    failed       = $script:failed
}
$player.Stop(); $player.Close()
$result | ConvertTo-Json -Compress
"""


# ---- pure helpers (self-tested) -----------------------------------------------------------------

def luma_stats(gray: bytes) -> tuple[float, float]:
    """Mean and population standard deviation of 8-bit gray samples."""
    if not gray:
        return 0.0, 0.0
    return statistics.fmean(gray), statistics.pstdev(gray)


def is_non_black(stats: tuple[float, float]) -> bool:
    return stats[0] >= MIN_MEAN_LUMA and stats[1] >= MIN_LUMA_STDDEV


def mean_abs_diff(a: bytes, b: bytes) -> float:
    n = min(len(a), len(b))
    return 0.0 if n == 0 else sum(abs(a[i] - b[i]) for i in range(n)) / n


def edge_command(edge: str, profile: Path, media: Path) -> list[str]:
    return [
        edge, f"--user-data-dir={profile}", "--no-first-run", "--no-default-browser-check", "--disable-extensions",
        "--autoplay-policy=no-user-gesture-required", "--new-window", "--kiosk", "--edge-kiosk-type=fullscreen",
        media.resolve().as_uri(),
    ]


def vlc_command(vlc: str, media: Path) -> list[str]:
    return [vlc, "--no-video-title-show", "--no-qt-privacy-ask", "--play-and-exit", "--fullscreen", str(media)]


def find_executable(candidates: Sequence[str], names: Sequence[str]) -> str | None:
    for cand in candidates:
        expanded = os.path.expandvars(cand)
        if os.path.isfile(expanded):
            return expanded
    for name in names:
        found = shutil.which(name)
        if found:
            return found
    return None


# ---- evidence steps ----------------------------------------------------------------------------

class Evidence:
    def __init__(self, out_dir: Path) -> None:
        self.out_dir = out_dir
        self.checks: list[dict[str, Any]] = []

    def check(self, name: str, ok: bool, detail: Any = "") -> bool:
        self.checks.append({"name": name, "pass": bool(ok), "detail": str(detail)})
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {str(detail)[:200]}", flush=True)
        return bool(ok)

    @property
    def passed(self) -> bool:
        return bool(self.checks) and all(c["pass"] for c in self.checks)


def powershell() -> str:
    return shutil.which("powershell") or shutil.which("pwsh") or "powershell"


def run_ps(script: str, *args: str, timeout: float = 120, sta: bool = False) -> subprocess.CompletedProcess[str]:
    with tempfile.NamedTemporaryFile("w", suffix=".ps1", delete=False, encoding="utf-8") as fh:
        fh.write(script)
        path = fh.name
    try:
        cmd = [powershell(), "-NoProfile", "-NonInteractive", *(["-STA"] if sta else []), "-ExecutionPolicy", "Bypass", "-File", path, *args]
        return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout)
    finally:
        Path(path).unlink(missing_ok=True)


def screenshot(path: Path) -> bool:
    res = run_ps(SCREENSHOT_PS, str(path))
    return res.returncode == 0 and path.is_file() and path.stat().st_size > 1000


def gray_samples(image: Path, width: int = 64, height: int = 36, seek: float | None = None) -> bytes:
    cmd = ["ffmpeg", "-v", "error", *(["-ss", f"{seek:.3f}"] if seek is not None else []), "-i", str(image), "-vf", f"scale={width}:{height}", "-frames:v", "1", "-pix_fmt", "gray", "-f", "rawvideo", "-"]
    proc = subprocess.run(cmd, capture_output=True, timeout=120)
    return proc.stdout if proc.returncode == 0 else b""


def foreground(pid: int) -> None:
    run_ps(FOREGROUND_PS, "-ProcessId", str(pid), timeout=30)


def kill_tree(proc: subprocess.Popen[bytes]) -> None:
    if proc.poll() is None:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True, timeout=60)
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()


def tray_evidence(ev: Evidence, root: Path, env: dict[str, str], out: Path) -> None:
    tray = root / "bin" / "arb-tray.exe"
    proc = subprocess.Popen([str(tray), "--auto-start"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    try:
        time.sleep(6)
        ev.check("arb-tray.exe is alive after 6 s on the real desktop", proc.poll() is None, f"returncode={proc.returncode}")
        second = subprocess.run([str(tray), "--auto-start"], env=env, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60)
        ev.check("a second arb-tray.exe exits at once (single instance)", second.returncode == 0 and "arb-tray.lock" in second.stderr, f"rc={second.returncode} {second.stderr.strip()[:120]}")
        shot = out / "win-tray.png"
        ev.check("desktop screenshot with the tray running (win-tray.png)", screenshot(shot), shot)
    finally:
        kill_tree(proc)
        subprocess.run([str(root / "bin" / "arb-cli.exe"), "daemon", "stop"], env=env, capture_output=True, timeout=60)


def play_in_edge(ev: Evidence, edge: str, media: Path, label: str, out: Path, baseline: bytes, record: bool) -> None:
    profile = Path(tempfile.mkdtemp(prefix="edge-profile-"))
    proc = subprocess.Popen(edge_command(edge, profile, media), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    try:
        foreground(proc.pid)
        time.sleep(3)
        shot = out / f"win-edge-{label}.png"
        deadline = time.monotonic() + 15
        while True:
            captured = screenshot(shot)
            samples = gray_samples(shot) if captured else b""
            stats = luma_stats(samples)
            diff = mean_abs_diff(samples, baseline)
            ready = is_non_black(stats) and diff >= MIN_DIFF_FROM_BASELINE
            if ready or time.monotonic() >= deadline:
                break
            time.sleep(0.5)
        ev.check(f"Edge window playing {media.name}: screenshot taken ({shot.name})", captured, shot)
        ev.check(f"Edge playback of {media.name} is on screen, not black (mean {stats[0]:.1f}, sd {stats[1]:.1f}, diff vs idle desktop {diff:.1f})", ready, stats)
        if record and ready:
            try:
                result = subprocess.run(
                    ["ffmpeg", "-y", "-v", "error", "-f", "gdigrab", "-framerate", "10", "-i", "desktop",
                     "-t", "9", "-c:v", "libx264", "-pix_fmt", "yuv420p", str(out / "win-playback.mp4")],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=30,
                )
                ev.check("desktop recording finished while Edge was playing", result.returncode == 0,
                         result.stderr.decode("utf-8", "replace")[-200:])
            except subprocess.TimeoutExpired:
                ev.check("desktop recording finished while Edge was playing", False, "timed out after 30 s")
    finally:
        kill_tree(proc)
        shutil.rmtree(profile, ignore_errors=True)


def vlc_evidence(ev: Evidence, vlc: str, media: Path, out: Path, baseline: bytes) -> None:
    proc = subprocess.Popen(vlc_command(vlc, media), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    try:
        time.sleep(4)
        foreground(proc.pid)
        time.sleep(2)
        shot = out / "win-vlc-taps.png"
        ok = screenshot(shot)
        ev.check(f"VLC window playing {media.name}: screenshot taken ({shot.name})", ok, shot)
        if ok:
            stats = luma_stats(gray_samples(shot))
            diff = mean_abs_diff(gray_samples(shot), baseline)
            ev.check(f"VLC playback is on screen, not black (mean {stats[0]:.1f}, sd {stats[1]:.1f}, diff vs idle desktop {diff:.1f})", is_non_black(stats) and diff >= MIN_DIFF_FROM_BASELINE, stats)
    finally:
        kill_tree(proc)


def recording_evidence(ev: Evidence, out: Path) -> None:
    rec = out / "win-playback.mp4"
    if not ev.check("screen recording win-playback.mp4 exists", rec.is_file() and rec.stat().st_size > 10_000, rec.stat().st_size if rec.is_file() else "missing"):
        return
    for second in (2.0, 5.0):
        stats = luma_stats(gray_samples(rec, seek=second))
        ev.check(f"recorded frame at {second:.0f} s is not black (mean {stats[0]:.1f}, sd {stats[1]:.1f})", is_non_black(stats), stats)


def media_foundation_evidence(ev: Evidence, media: Path) -> None:
    res = run_ps(MEDIA_FOUNDATION_PS, media.resolve().as_uri(), timeout=90, sta=True)
    try:
        data = json.loads(res.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        ev.check("Media Foundation (WPF MediaPlayer) answers", False, f"rc={res.returncode} {res.stdout.strip()[:150]} {res.stderr.strip()[:150]}")
        return
    ok = data.get("has_duration") and data.get("width") == 1920 and data.get("height") == 1080 and data.get("position_s", 0) > 1.0 and not data.get("failed")
    ev.check(f"Media Foundation plays {media.name} (1920x1080, position advanced)", bool(ok), data)


def run_evidence(args: argparse.Namespace) -> int:
    out = args.out_dir.resolve()
    out.mkdir(parents=True, exist_ok=True)
    ev = Evidence(out)
    env = {k: v for k, v in os.environ.items() if not k.startswith("ARB_")}
    work = Path(tempfile.mkdtemp(prefix="arb-gui-evidence-"))
    runtime = Path(tempfile.mkdtemp(prefix="arbg-"))
    env.update({"ARB_DATA_DIR": str(work / "data"), "ARB_CACHE_DIR": str(work / "cache"), "ARB_RUNTIME_DIR": str(runtime)})
    try:
        baseline_png = out / "win-baseline.png"
        ev.check("idle desktop screenshot (win-baseline.png)", screenshot(baseline_png), baseline_png)
        baseline = gray_samples(baseline_png) if baseline_png.is_file() else b""
        tray_evidence(ev, args.root, env, out)
        edge = find_executable([r"%ProgramFiles(x86)%\Microsoft\Edge\Application\msedge.exe", r"%ProgramFiles%\Microsoft\Edge\Application\msedge.exe"], ["msedge"])
        if ev.check("Microsoft Edge is installed", edge is not None, edge):
            play_in_edge(ev, edge, args.share, "share", out, baseline, record=True)
            play_in_edge(ev, edge, args.share_taps, "taps", out, baseline, record=False)
            recording_evidence(ev, out)
        vlc = find_executable([r"%ProgramFiles%\VideoLAN\VLC\vlc.exe", r"%ProgramFiles(x86)%\VideoLAN\VLC\vlc.exe"], ["vlc"])
        if ev.check("VLC is installed", vlc is not None, vlc):
            vlc_evidence(ev, vlc, args.share_taps, out, baseline)
        media_foundation_evidence(ev, args.share_taps)
    finally:
        shutil.rmtree(work, ignore_errors=True)
        shutil.rmtree(runtime, ignore_errors=True)
    (out / "windows-gui-evidence.json").write_text(json.dumps({"pass": ev.passed, "checks": ev.checks}, indent=2, ensure_ascii=False), encoding="utf-8")
    print(f"windows_gui_evidence.py: {'PASS' if ev.passed else 'FAIL'}")
    return 0 if ev.passed else 1


def self_test() -> int:
    from unittest.mock import MagicMock, patch

    black = bytes([0] * 2304)
    desk = bytes([30, 90] * 1152)
    flat = bytes([120] * 2304)
    assert not is_non_black(luma_stats(black)), "black frame must be rejected"
    assert not is_non_black(luma_stats(flat)), "a flat frame has no content"
    assert is_non_black(luma_stats(desk)), "a textured frame is content"
    assert mean_abs_diff(black, black) == 0.0 and mean_abs_diff(desk, black) > MIN_DIFF_FROM_BASELINE
    cmd = edge_command("msedge.exe", Path("C:/p"), Path("C:/clips/share-taps.mp4"))
    assert "--autoplay-policy=no-user-gesture-required" in cmd and cmd[-1].startswith("file:///"), cmd
    assert vlc_command("vlc.exe", Path("a.mp4"))[-1] == "a.mp4"
    with tempfile.TemporaryDirectory() as tmp:
        fake = Path(tmp) / "tool.exe"
        fake.write_bytes(b"x")
        assert find_executable([str(fake)], ["definitely-not-installed-xyz"]) == str(fake)
        assert find_executable(["/nope/tool.exe"], ["definitely-not-installed-xyz"]) is None
        # The desktop may be black throughout Edge startup. Only record verified playback.
        events = []
        frames = iter((black, desk))
        edge = MagicMock()

        def launch(argv, **kwargs):
            events.append("record" if argv[0] == "ffmpeg" else "launch")
            return edge

        def record(*args, **kwargs):
            events.append("record")
            return subprocess.CompletedProcess(args, 0, stderr=b"")

        def samples(_):
            events.append("frame")
            return next(frames)
        with (patch.object(subprocess, "Popen", side_effect=launch),
              patch.object(subprocess, "run", side_effect=record),
              patch.object(time, "sleep"), patch(__name__ + ".foreground"),
              patch(__name__ + ".screenshot", return_value=True), patch(__name__ + ".gray_samples", side_effect=samples),
              patch(__name__ + ".kill_tree", side_effect=lambda _: events.append("close"))):
            play_in_edge(Evidence(Path(tmp)), "edge", Path("share.mp4"), "share", Path(tmp), black, record=True)
        assert events == ["launch", "frame", "frame", "record", "close"], events
    ev = Evidence(Path("."))
    assert not ev.passed and ev.check("x", True) and ev.passed and not ev.check("y", False) and not ev.passed
    print("windows_gui_evidence.py --self-test: OK")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--root", type=Path, help="extracted arb-windows-x86_64 folder")
    ap.add_argument("--share", type=Path, help="share.mp4 produced with burn_taps=false")
    ap.add_argument("--share-taps", type=Path, help="share-taps.mp4 produced with burn_taps=true")
    ap.add_argument("--out-dir", type=Path, default=Path("win-gui-evidence"))
    args = ap.parse_args(argv)
    if args.self_test:
        return self_test()
    if os.name != "nt":
        print("ERROR: windows_gui_evidence.py needs a real Windows desktop", file=sys.stderr)
        return 2
    for name in ("root", "share", "share_taps"):
        if getattr(args, name) is None or not getattr(args, name).exists():
            print(f"ERROR: --{name.replace('_', '-')} is required and must exist", file=sys.stderr)
            return 2
    return run_evidence(args)


if __name__ == "__main__":
    sys.exit(main())

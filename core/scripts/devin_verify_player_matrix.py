#!/usr/bin/env python3
"""Player & media verification helper for Devin multi-OS testing (VER-11).

Modes
  --self-test                       verify helper logic (no external tools needed except optional ffmpeg)
  --generate-fixture OUT.mp4        FHD30 H.264 fixture (burned-in frame numbers + top-left barcode)
  --mp4 PATH                        verify an already trimmed MP4 (expects 601 frames by default)
  --bundle B --arb-cli BIN          run `arb-cli trim B --in-frame 47 --out-frame 647`, then verify
                                    (+ `scrub-bench`); extra global args via --cli-arg (repeatable)

Outputs (in --out-dir): devin-report.json, devin-report.md, frame-0.png, frame-last.png
Exit code 0 only when every mandatory check passes.
"""
from __future__ import annotations

import argparse
import json
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

EXPECTED_FRAMES = 601
TRIM_IN, TRIM_OUT = 47, 647


def run(cmd, timeout=600):
    # Decode tool output as UTF-8 explicitly: the OS default codepage (cp1252
    # on Windows CI) raises UnicodeDecodeError on non-ASCII tool output.
    p = subprocess.run(
        [str(c) for c in cmd],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
    )
    return p.returncode, p.stdout, p.stderr


def check(name, ok, detail=""):
    return {"name": name, "pass": bool(ok), "detail": str(detail)}


# ---- pure helpers (self-tested) -------------------------------------------------

def parse_boxes(data: bytes):
    """Top-level ISO BMFF box list [(type, offset, size)]."""
    boxes, pos = [], 0
    while pos + 8 <= len(data):
        size = int.from_bytes(data[pos:pos + 4], "big")
        typ = data[pos + 4:pos + 8].decode("latin1")
        hdr = 8
        if size == 1:
            size = int.from_bytes(data[pos + 8:pos + 16], "big")
            hdr = 16
        elif size == 0:
            size = len(data) - pos
        if size < hdr:
            break
        boxes.append((typ, pos, size))
        pos += size
    return boxes


def iter_boxes(data: bytes, start=0, end=None, depth=0):
    """Yield (type, offset, size, header_len) for every box in [start, end), recursing into
    container boxes. Handles 64-bit extended headers (size == 1) and size == 0 (to end)."""
    end = len(data) if end is None else end
    containers = {"moov", "trak", "mdia", "minf", "stbl", "edts", "udta", "dinf"}
    pos = start
    while pos + 8 <= end:
        size = int.from_bytes(data[pos:pos + 4], "big")
        typ = data[pos + 4:pos + 8].decode("latin1")
        hdr = 8
        if size == 1:
            if pos + 16 > end:
                break
            size = int.from_bytes(data[pos + 8:pos + 16], "big")
            hdr = 16
        elif size == 0:
            size = end - pos
        if size < hdr:
            break
        yield typ, pos, size, hdr
        if typ in containers and depth < 8:
            yield from iter_boxes(data, pos + hdr, min(pos + size, end), depth + 1)
        pos += size


def find_box_recursive(data: bytes, wanted: str, start=0, end=None, depth=0):
    """Count occurrences of a box type inside known container boxes (64-bit-size safe)."""
    return sum(1 for typ, _p, _s, _h in iter_boxes(data, start, end, depth) if typ == wanted)


def faststart(boxes):
    types = [b[0] for b in boxes]
    return "moov" in types and "mdat" in types and types.index("moov") < types.index("mdat")


def stsd_avc1_count(data: bytes):
    """Count avc1/avc3 *sample entries* declared inside `stsd` boxes.

    Walks the box tree properly instead of raw `data.count(b"avc1")` — byte patterns inside
    `mdat` can never be miscounted as codec entries (R4 C3)."""
    avc1 = avc3 = 0
    for typ, pos, size, hdr in iter_boxes(data):
        if typ != "stsd":
            continue
        # stsd is a FullBox: version/flags (4B) + entry_count (4B) precede the entries.
        p, limit = pos + hdr + 8, pos + size
        while p + 8 <= limit:
            entry_size = int.from_bytes(data[p:p + 4], "big")
            if entry_size < 8:
                break
            entry_type = data[p + 4:p + 8]
            if entry_type == b"avc1":
                avc1 += 1
            elif entry_type == b"avc3":
                avc3 += 1
            p += entry_size
    return avc1, avc3


def safe_float(value, default=0.0):
    """Parse ffprobe numeric fields; `"N/A"`/missing values become `default` (R4 C3)."""
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def negative_ts(pkt_rows):
    return [r for r in pkt_rows if any(safe_float(r.get(k, 0) or 0) < 0 for k in ("pts_time", "dts_time"))]


def machine_barcode(n: int, bits: int = 16):
    """MSB-first list of bits encoding frame number n (barcode cells)."""
    return [(n >> (bits - 1 - i)) & 1 for i in range(bits)]


def decode_barcode(bits):
    v = 0
    for b in bits:
        v = (v << 1) | b
    return v


def md_report(report):
    lines = ["# Devin verification report", "",
             f"- host: `{report['host']}`", f"- input: `{report['input']}`",
             f"- overall: **{'PASS' if report['pass'] else 'FAIL'}**", "",
             "| check | result | detail |", "| --- | --- | --- |"]
    for c in report["checks"]:
        lines.append(f"| {c['name']} | {'PASS' if c['pass'] else 'FAIL'} | {c['detail']} |")
    lines += ["", "## Players", ""]
    for p in report["players"]:
        lines.append(f"- {p['player']}: {p['status']} {p.get('detail', '')}")
    return "\n".join(lines) + "\n"


# ---- fixture ---------------------------------------------------------------------

def generate_fixture(out: Path, gop_seconds: int = 1, frames: int = EXPECTED_FRAMES, first_number: int = TRIM_IN):
    """FHD30 fixture; numbering starts at 47 so frame 0/last read 47 / 647 like a trimmed clip.

    Burned-in decimal number needs ffmpeg's drawtext filter (freetype); the 16-cell machine barcode
    (top-left, 32px cells, MSB first) uses drawbox only and is always present.
    """
    if not shutil.which("ffmpeg"):
        raise RuntimeError("ffmpeg not found")
    gop = 30 * gop_seconds
    _, filters, _ = run(["ffmpeg", "-hide_banner", "-filters"])
    parts = []
    if " drawtext " in filters:
        parts.append(f"drawtext=text='%{{eif\\:n+{first_number}\\:d}}':x=(w-tw)/2:y=(h-th)/2:fontsize=220:"
                     "fontcolor=white:box=1:boxcolor=black@0.6")
    for i in range(16):
        parts.append(f"drawbox=x={i*32}:y=0:w=32:h=32:color=white:t=fill:"
                     f"enable='gte(mod(floor((n+{first_number})/pow(2,{15-i})),2),1)'")
    cmd = ["ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", f"testsrc2=size=1920x1080:rate=30:duration={frames/30+1}",
           "-vf", ",".join(parts), "-frames:v", str(frames), "-c:v", "libx264", "-preset", "veryfast",
           "-x264-params", "repeat-headers=1", "-g", str(gop), "-keyint_min", str(gop), "-sc_threshold", "0",
           "-bf", "0", "-pix_fmt", "yuv420p", "-use_editlist", "0", "-movflags", "+faststart", str(out)]
    rc, _, err = run(cmd)
    if rc != 0:
        raise RuntimeError(f"fixture generation failed: {err[-400:]}")
    return out


# ---- verification ----------------------------------------------------------------

def ffprobe_json(path, *args):
    rc, out, err = run(["ffprobe", "-v", "error", "-of", "json", *args, str(path)])
    if rc != 0:
        raise RuntimeError(err)
    return json.loads(out)


_FPS_MODE_ARGS = None


def fps_mode_args():
    """`-fps_mode vfr` on FFmpeg 5.1+, else the equivalent `-vsync 0` (FFmpeg 4.x)."""
    global _FPS_MODE_ARGS
    if _FPS_MODE_ARGS is None:
        _FPS_MODE_ARGS = ["-fps_mode", "vfr"]
        try:
            rc, out, err = run(["ffmpeg", "-hide_banner", "-h", "full"])
            if rc != 0 or "-fps_mode" not in out + err:
                _FPS_MODE_ARGS = ["-vsync", "0"]
        except Exception:
            _FPS_MODE_ARGS = ["-vsync", "0"]
    return _FPS_MODE_ARGS


def verify_mp4(path: Path, expected_frames: int, outdir: Path):
    checks = []
    data = path.read_bytes()
    boxes = parse_boxes(data)
    checks.append(check("faststart moov-before-mdat", faststart(boxes), [b[0] for b in boxes]))
    checks.append(check("0 edts", find_box_recursive(data, "edts") == 0, find_box_recursive(data, "edts")))
    checks.append(check("0 elst", find_box_recursive(data, "elst") == 0, find_box_recursive(data, "elst")))
    avc1, avc3 = stsd_avc1_count(data)
    checks.append(check("single-stsd avc1 (no avc3)", avc1 >= 1 and avc3 == 0, f"avc1={avc1} avc3={avc3}"))
    info = ffprobe_json(path, "-select_streams", "v:0", "-count_frames",
                        "-show_entries", "stream=codec_name,width,height,nb_read_frames,avg_frame_rate")
    s = (info.get("streams") or [{}])[0]
    checks.append(check("codec h264", s.get("codec_name") == "h264", s.get("codec_name")))
    nb_frames = int(safe_float(s.get("nb_read_frames"), -1))
    checks.append(check(f"exactly {expected_frames} frames", nb_frames == expected_frames, s.get("nb_read_frames")))
    pk = ffprobe_json(path, "-select_streams", "v:0", "-show_entries", "packet=pts_time,dts_time,flags")
    pkts = pk.get("packets", [])
    checks.append(check("0 negative PTS/DTS", not negative_ts(pkts), f"{len(negative_ts(pkts))} negative"))
    checks.append(check("first packet is keyframe", bool(pkts) and "K" in pkts[0].get("flags", ""), pkts[0].get("flags") if pkts else "none"))
    # Full-stream ffmpeg decode verifying 0 decode errors across every frame.
    # `-fps_mode vfr` preserves the track's native 90kHz timebase on VFR Android screen
    # captures so ffmpeg's `null` muxer does not quantize adjacent VFR frames onto the same tick.
    rc_full, _, e_full = run(["ffmpeg", "-v", "error", "-i", path, *fps_mode_args(), "-f", "null", "-"])
    checks.append(check("ffmpeg full-stream decode (0 errors)", rc_full == 0 and not e_full.strip(), e_full[-200:]))
    # frame 0 / last PNG via ffmpeg (decoder reference).
    # For the last frame we seek to the final keyframe and use `-update 1` rather than
    # `-vf select=eq(n,last)`: when a mid-GOP trimmed MP4 splices a re-encoded head GOP onto
    # a stream-copied tail GOP whose VUI color tags differ, ffmpeg reconfigures its filter
    # graph at the splice boundary and resets `select`'s `n` counter back to 0.
    f0, fl = outdir / "frame-0.png", outdir / "frame-last.png"
    rc0, _, e0 = run(["ffmpeg", "-y", "-v", "error", "-i", path, *fps_mode_args(), "-vframes", "1", f0])
    last_kf_pts = 0.0
    for pkt in pkts:
        if "K" in pkt.get("flags", ""):
            last_kf_pts = max(0.0, safe_float(pkt.get("pts_time"), last_kf_pts))
    seek_sec = max(0.0, last_kf_pts - 0.001)
    rcl, _, el = run([
        "ffmpeg", "-y", "-v", "error", "-ss", f"{seek_sec:.6f}", "-i", path,
        *fps_mode_args(), "-update", "1", fl,
    ])
    checks.append(check("ffmpeg decode frame 0 -> PNG", rc0 == 0 and f0.exists() and f0.stat().st_size > 0, e0[-200:]))
    checks.append(check(f"ffmpeg decode frame {expected_frames-1} -> PNG", rcl == 0 and fl.exists() and fl.stat().st_size > 0, el[-200:]))
    return checks


def verify_audio_m4a(path: Path):
    """Verify a companion AAC-LC `audio.m4a` track (faststart, 0 edts/elst, ffprobe/ffmpeg 0 errors)."""
    checks = []
    data = path.read_bytes()
    boxes = parse_boxes(data)
    checks.append(check("audio.m4a faststart moov-before-mdat", faststart(boxes), [b[0] for b in boxes]))
    checks.append(check("audio.m4a 0 edts", find_box_recursive(data, "edts") == 0, find_box_recursive(data, "edts")))
    checks.append(check("audio.m4a 0 elst", find_box_recursive(data, "elst") == 0, find_box_recursive(data, "elst")))
    info = ffprobe_json(path, "-select_streams", "a:0", "-count_packets",
                        "-show_entries", "stream=codec_name,profile,sample_rate,channels,nb_read_packets")
    s = (info.get("streams") or [{}])[0]
    checks.append(check("audio.m4a codec aac", s.get("codec_name") == "aac", f"{s.get('codec_name')} ({s.get('profile')})"))
    nb_pkts = int(safe_float(s.get("nb_read_packets"), -1))
    checks.append(check("audio.m4a packets > 0", nb_pkts > 0, f"{nb_pkts} packets, {s.get('sample_rate')}Hz {s.get('channels')}ch"))
    rc, _, err = run(["ffmpeg", "-v", "error", "-i", path, "-f", "null", "-"])
    checks.append(check("audio.m4a ffmpeg decode (0 errors)", rc == 0 and not err.strip(), err[-200:]))
    return checks


def probe_players(path: Path, outdir: Path):
    """Best-effort local player probes. 'MANUAL' means Devin must record video per the test plan."""
    sysname = platform.system()
    players = []
    for tool in ("ffmpeg", "ffplay", "vlc", "cvlc"):
        players.append({"player": tool, "status": "available" if shutil.which(tool) else "missing"})
    if sysname == "Darwin":
        players.append({"player": "QuickTime Player", "status": "MANUAL", "detail": f"open -a 'QuickTime Player' {path}  (record screen)"})
        players.append({"player": "Chrome", "status": "MANUAL", "detail": f"open -a 'Google Chrome' {path}"})
    elif sysname == "Windows":
        players.append({"player": "Windows Media Player / Movies & TV", "status": "MANUAL", "detail": f"start {path}"})
        players.append({"player": "Chrome", "status": "MANUAL", "detail": f"start chrome {path}"})
    else:
        players.append({"player": "Chrome", "status": "MANUAL", "detail": f"google-chrome {path}"})
    if shutil.which("vlc") or shutil.which("cvlc"):
        exe = shutil.which("cvlc") or shutil.which("vlc")
        snap = outdir / "vlc-snap"
        snap.mkdir(exist_ok=True)
        rc, _, err = run([exe, "--intf", "dummy", "--play-and-exit", "--video-filter=scene", "--scene-format=png",
                          f"--scene-path={snap}", "--scene-ratio=300", str(path)], timeout=120)
        pngs = list(snap.glob("*.png"))
        players.append({"player": "vlc headless decode", "status": "PASS" if rc == 0 and pngs else "FAIL", "detail": f"{len(pngs)} snapshots"})
    return players


def cli_command(arb_cli, cli_args, subcommand, *rest):
    """Build an arb-cli argv: global flags (--cli-arg, --json) always precede the subcommand."""
    return [arb_cli, *cli_args, "--json", subcommand, *rest]


def trim_via_cli(args, outdir: Path):
    rc, out, err = run(cli_command(args.arb_cli, args.cli_arg, "scrub-bench", args.bundle))
    scrub = out.strip()
    rc2, out2, err2 = run(cli_command(args.arb_cli, args.cli_arg, "trim", args.bundle,
                                      "--in-frame", TRIM_IN, "--out-frame", TRIM_OUT, "--dest", outdir))
    if rc2 != 0:
        raise RuntimeError(f"arb-cli trim failed: {err2[-400:]}")
    j = json.loads(out2)
    mp4 = j.get("trim_report", {}).get("output_path") or j.get("output_path")
    if not mp4:
        cands = sorted(outdir.glob("*.mp4"))
        if not cands:
            raise RuntimeError("trim produced no mp4: " + out2[:400])
        mp4 = cands[-1]
    return Path(mp4), scrub, rc == 0


# ---- self-test -------------------------------------------------------------------

def self_test():
    failures = []

    def require(name, cond, detail=""):
        if not cond:
            failures.append(f"{name}: {detail}")

    require("barcode roundtrip", decode_barcode(machine_barcode(647)) == 647)
    require("barcode bits", machine_barcode(1, 4) == [0, 0, 0, 1])

    def box(t, payload=b""):
        return (8 + len(payload)).to_bytes(4, "big") + t.encode() + payload

    def box64(t, payload=b""):
        # 64-bit extended-size box: size field 1, then an 8-byte largesize.
        return (1).to_bytes(4, "big") + t.encode() + (16 + len(payload)).to_bytes(8, "big") + payload

    stsd_avc1 = box("stsd", b"\x00\x00\x00\x00" + (1).to_bytes(4, "big") + box("avc1", b"\x00" * 78))
    good = box("ftyp", b"isom") + box("moov", box("trak", box("mdia", box("minf", box("stbl", stsd_avc1))))) + box("mdat", b"x")
    require("faststart", faststart(parse_boxes(good)))
    require("no edts", find_box_recursive(good, "edts") == 0)
    bad = box("ftyp") + box("moov", box("trak", box("edts", box("elst")))) + box("mdat")
    require("edts detected", find_box_recursive(bad, "edts") == 1 and find_box_recursive(bad, "elst") == 1)
    require("not faststart", not faststart(parse_boxes(box("ftyp") + box("mdat") + box("moov"))))
    require("stsd avc1 walk", stsd_avc1_count(good) == (1, 0), stsd_avc1_count(good))
    # avc3 sample entry must be rejected (HEVC-in-AVC is disallowed in our bundles).
    stsd_avc3 = box("stsd", b"\x00\x00\x00\x00" + (1).to_bytes(4, "big") + box("avc3", b"\x00" * 78))
    with_avc3 = box("ftyp") + box("moov", box("trak", box("mdia", box("minf", box("stbl", stsd_avc3))))) + box("mdat")
    require("stsd avc3 walk", stsd_avc1_count(with_avc3) == (0, 1), stsd_avc1_count(with_avc3))
    # avc1 bytes inside mdat payload must NOT be counted (raw data.count bug, R4 C3).
    false_positive = box("ftyp") + box("moov", box("trak", box("mdia", box("minf", box("stbl", stsd_avc1))))) + box("mdat", b"avc1avc3")
    require("mdat bytes not counted", stsd_avc1_count(false_positive) == (1, 0), stsd_avc1_count(false_positive))
    # 64-bit extended-size containers must still be walked (R4 C3).
    good64 = box("ftyp") + box64("moov", box("trak", box("edts", box("elst")))) + box("mdat")
    require("64-bit box walk", find_box_recursive(good64, "edts") == 1 and find_box_recursive(good64, "elst") == 1)
    # ffprobe "N/A" / missing pts fields must not raise (R4 C3).
    require("negative ts", negative_ts([{"pts_time": "-0.033", "dts_time": "0"}, {"pts_time": "0", "dts_time": "0"}]))
    require("clean ts", not negative_ts([{"pts_time": "0", "dts_time": "0"}]))
    require("N/A ts safe", not negative_ts([{"pts_time": "N/A"}, {"dts_time": "N/A"}, {}]))
    require("safe_float", safe_float("N/A") == 0.0 and safe_float(None, -1) == -1 and safe_float("2.5") == 2.5)
    cmd = cli_command("arb-cli", ["--data-dir=/d", "--runtime-dir=/r"], "trim", "B")
    require("global --cli-arg before trim", cmd.index("--data-dir=/d") < cmd.index("trim") and cmd.index("--runtime-dir=/r") < cmd.index("trim") and cmd.index("--json") < cmd.index("trim"), cmd)
    require("fps_mode fallback args", fps_mode_args() in (["-fps_mode", "vfr"], ["-vsync", "0"]), fps_mode_args())
    r = {"host": "h", "input": "i", "pass": True, "checks": [check("a", True, "d")], "players": [{"player": "p", "status": "ok"}]}
    require("md report", "PASS" in md_report(r))
    msg = "helper logic OK"
    if shutil.which("ffmpeg") and shutil.which("ffprobe"):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            # tiny fixture: 31 frames at 160x90 to keep self-test fast
            rc, _, err = run(["ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=30:duration=2",
                              "-frames:v", "31", "-c:v", "libx264", "-g", "30", "-bf", "0", "-x264-params", "repeat-headers=1",
                              "-pix_fmt", "yuv420p", "-use_editlist", "0", "-movflags", "+faststart", str(td / "t.mp4")])
            require("ffmpeg fixture", rc == 0, err[-300:])
            if rc == 0:
                failed = [c for c in verify_mp4(td / "t.mp4", 31, td) if not c["pass"]]
                require("verify_mp4", not failed, failed)
            if not failures:
                msg += "; ffmpeg/ffprobe pipeline OK"
    else:
        msg += "; ffmpeg/ffprobe missing -> media pipeline skipped"
    if failures:
        print("SELF-TEST FAILED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print("SELF-TEST PASSED:", msg)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--generate-fixture", metavar="OUT.mp4")
    ap.add_argument("--gop-seconds", type=int, default=1, choices=(1, 5))
    ap.add_argument("--mp4")
    ap.add_argument("--bundle")
    ap.add_argument("--arb-cli", default="arb-cli")
    ap.add_argument("--cli-arg", action="append", default=[], help="extra global arb-cli arg, e.g. --cli-arg=--data-dir=/x")
    ap.add_argument("--expected-frames", type=int, default=EXPECTED_FRAMES)
    ap.add_argument("--out-dir", default="devin-out")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    outdir = Path(a.out_dir)
    outdir.mkdir(parents=True, exist_ok=True)
    if a.generate_fixture:
        print(generate_fixture(Path(a.generate_fixture), a.gop_seconds))
        return
    extra = {}
    if a.bundle:
        mp4, scrub, ok = trim_via_cli(a, outdir)
        extra["scrub_bench"] = scrub
    elif a.mp4:
        mp4 = Path(a.mp4)
    else:
        ap.error("one of --self-test, --generate-fixture, --mp4, --bundle is required")
    checks = verify_mp4(mp4, a.expected_frames, outdir)
    audio_m4a = mp4.with_name("audio.m4a")
    if audio_m4a.is_file():
        checks.extend(verify_audio_m4a(audio_m4a))
    players = probe_players(mp4, outdir)
    ok = all(c["pass"] for c in checks) and not any(p["status"] == "FAIL" for p in players)
    report = {"host": f"{platform.system()} {platform.release()} {platform.machine()}", "input": str(mp4),
              "pass": ok, "checks": checks, "players": players, **extra}
    (outdir / "devin-report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
    (outdir / "devin-report.md").write_text(md_report(report))
    print(md_report(report))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()

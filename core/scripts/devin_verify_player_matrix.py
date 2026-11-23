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
    p = subprocess.run([str(c) for c in cmd], capture_output=True, text=True, timeout=timeout)
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


def find_box_recursive(data: bytes, wanted: str, start=0, end=None, depth=0):
    """Count occurrences of a box type inside known container boxes."""
    end = len(data) if end is None else end
    containers = {"moov", "trak", "mdia", "minf", "stbl", "edts", "udta", "dinf"}
    n, pos = 0, start
    while pos + 8 <= end:
        size = int.from_bytes(data[pos:pos + 4], "big")
        typ = data[pos + 4:pos + 8].decode("latin1")
        if size < 8:
            break
        if typ == wanted:
            n += 1
        if typ in containers and depth < 8:
            n += find_box_recursive(data, wanted, pos + 8, min(pos + size, end), depth + 1)
        pos += size
    return n


def faststart(boxes):
    types = [b[0] for b in boxes]
    return "moov" in types and "mdat" in types and types.index("moov") < types.index("mdat")


def stsd_avc1_count(data: bytes):
    return data.count(b"avc1"), data.count(b"avc3")


def negative_ts(pkt_rows):
    return [r for r in pkt_rows if any(float(r.get(k, 0) or 0) < 0 for k in ("pts_time", "dts_time"))]


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
    s = info["streams"][0]
    checks.append(check("codec h264", s["codec_name"] == "h264", s["codec_name"]))
    checks.append(check(f"exactly {expected_frames} frames", int(s["nb_read_frames"]) == expected_frames, s["nb_read_frames"]))
    pk = ffprobe_json(path, "-select_streams", "v:0", "-show_entries", "packet=pts_time,dts_time,flags")
    pkts = pk.get("packets", [])
    checks.append(check("0 negative PTS/DTS", not negative_ts(pkts), f"{len(negative_ts(pkts))} negative"))
    checks.append(check("first packet is keyframe", bool(pkts) and "K" in pkts[0].get("flags", ""), pkts[0].get("flags") if pkts else "none"))
    # frame 0 / last PNG via ffmpeg (decoder reference)
    f0, fl = outdir / "frame-0.png", outdir / "frame-last.png"
    rc0, _, e0 = run(["ffmpeg", "-y", "-v", "error", "-i", path, "-vf", "select=eq(n\\,0)", "-vframes", "1", f0])
    rcl, _, el = run(["ffmpeg", "-y", "-v", "error", "-i", path, "-vf", f"select=eq(n\\,{expected_frames-1})", "-vframes", "1", fl])
    checks.append(check("ffmpeg decode frame 0 -> PNG", rc0 == 0 and f0.exists() and f0.stat().st_size > 0, e0[-200:]))
    checks.append(check(f"ffmpeg decode frame {expected_frames-1} -> PNG", rcl == 0 and fl.exists() and fl.stat().st_size > 0, el[-200:]))
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


def trim_via_cli(args, outdir: Path):
    base = [args.arb_cli, *args.cli_arg, "--json"]
    rc, out, err = run([*base, "scrub-bench", args.bundle])
    scrub = out.strip()
    rc2, out2, err2 = run([*base, "trim", args.bundle, "--in-frame", TRIM_IN, "--out-frame", TRIM_OUT, "--dest", outdir])
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
    assert decode_barcode(machine_barcode(647)) == 647
    assert machine_barcode(1, 4) == [0, 0, 0, 1]
    def box(t, payload=b""):
        return (8 + len(payload)).to_bytes(4, "big") + t.encode() + payload
    good = box("ftyp", b"isom") + box("moov", box("trak", box("mdia", box("minf", box("stbl", box("stsd", b"avc1")))))) + box("mdat", b"x")
    assert faststart(parse_boxes(good))
    assert find_box_recursive(good, "edts") == 0
    bad = box("ftyp") + box("moov", box("trak", box("edts", box("elst")))) + box("mdat")
    assert find_box_recursive(bad, "edts") == 1 and find_box_recursive(bad, "elst") == 1
    assert not faststart(parse_boxes(box("ftyp") + box("mdat") + box("moov")))
    assert stsd_avc1_count(good) == (1, 0)
    assert negative_ts([{"pts_time": "-0.033", "dts_time": "0"}, {"pts_time": "0", "dts_time": "0"}])
    assert not negative_ts([{"pts_time": "0", "dts_time": "0"}])
    r = {"host": "h", "input": "i", "pass": True, "checks": [check("a", True, "d")], "players": [{"player": "p", "status": "ok"}]}
    assert "PASS" in md_report(r)
    msg = "helper logic OK"
    if shutil.which("ffmpeg") and shutil.which("ffprobe"):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            # tiny fixture: 31 frames at 160x90 to keep self-test fast
            rc, _, err = run(["ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=30:duration=2",
                              "-frames:v", "31", "-c:v", "libx264", "-g", "30", "-bf", "0", "-x264-params", "repeat-headers=1",
                              "-pix_fmt", "yuv420p", "-use_editlist", "0", "-movflags", "+faststart", str(td / "t.mp4")])
            assert rc == 0, err
            checks = verify_mp4(td / "t.mp4", 31, td)
            failed = [c for c in checks if not c["pass"]]
            assert not failed, failed
            msg += "; ffmpeg/ffprobe pipeline OK"
    else:
        msg += "; ffmpeg/ffprobe missing -> media pipeline skipped"
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

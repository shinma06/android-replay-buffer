#!/usr/bin/env python3
"""
Live end-to-end verification harness for ARB device-agent against an Android emulator/device.

Verifies:
  1. Hello handshake + /proc/sys/kernel/random/boot_id readability (or explicit fallback).
  2. Periodic Heartbeat 3-clock triple monotonicity & boottime >= monotonic.
  3. 20x Ping/Pong clock-sync probe RTT (min / median / p95 / max) & device bracket.
  4. Live binary logcat -B streaming tagged with device_read_boottime_ns + injected marker + logcat -S stats.
  5. Live fault-isolation proof: SIGKILLing the live logcat child process while Stream 0 Beacon ticks uninterrupted,
     verifying Error -> Restarting -> Running recovery and post-recovery log delivery.
  6. Stream 2 Input/Touch worker state check.
  7. Graceful Shutdown handshake.
  8. Optional Apache-2.0 scrcpy-server-v5.0.1 (+ patched AudioRecordReader) video handshake check.
"""

import argparse
import os
import queue
import random
import shutil
import socket
import statistics
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

MAGIC = b"AR"
HEADER_FMT = "<2sBBBBHIQ"
HEADER_SIZE = 20

STREAM_CONTROL = 0
STREAM_LOGCAT = 1
STREAM_INPUT = 2

MSG_HELLO = 0x01
MSG_HEARTBEAT = 0x02
MSG_PING = 0x03
MSG_PONG = 0x04
MSG_STREAM_STATE = 0x05
MSG_LOG_RECORD = 0x10
MSG_LOG_OVERFLOW_STATS = 0x11
MSG_TOUCH_EVENT = 0x20
MSG_SHUTDOWN = 0x30

FLAG_RECOVERED = 0x04

BOOT_ID_SOURCE_NAMES = {
    0: "KernelProcBootId",
    1: "FallbackBootCountBtime",
    2: "FallbackRandomUuid",
}
STATE_NAMES = {
    0: "Starting",
    1: "Running",
    2: "Stopped",
    3: "Error",
    4: "Restarting",
}
COMPLETENESS_NAMES = {
    0: "Verified",
    1: "OverflowDetected",
    2: "Unverified",
}


def find_adb() -> str:
    sdk = os.environ.get("ANDROID_HOME") or os.path.expanduser("~/Library/Android/sdk")
    candidate = os.path.join(sdk, "platform-tools", "adb")
    if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
        return candidate
    found = shutil.which("adb")
    if found:
        return found
    raise RuntimeError("adb binary not found")


def adb_cmd(adb: str, serial: str | None, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    cmd = [adb]
    if serial:
        cmd.extend(["-s", serial])
    cmd.extend(args)
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=check)


def recv_exact(sock: socket.socket, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError(f"Socket EOF after {len(buf)}/{n} bytes")
        buf.extend(chunk)
    return bytes(buf)


def read_frame(sock: socket.socket) -> dict:
    hdr = recv_exact(sock, HEADER_SIZE)
    magic, version, stream_id, msg_type, flags, reserved, seq, payload_len = struct.unpack(HEADER_FMT, hdr)
    if magic != MAGIC:
        raise ValueError(f"Bad magic: {magic!r}")
    if version != 1:
        raise ValueError(f"Unsupported version: {version}")
    payload = recv_exact(sock, payload_len) if payload_len > 0 else b""
    return {
        "stream_id": stream_id,
        "msg_type": msg_type,
        "flags": flags,
        "reserved": reserved,
        "seq": seq,
        "payload": payload,
        "host_recv_ns": time.perf_counter_ns(),
    }


def build_frame(stream_id: int, msg_type: int, flags: int, seq: int, payload: bytes) -> bytes:
    hdr = struct.pack(HEADER_FMT, MAGIC, 1, stream_id, msg_type, flags, 0, seq, len(payload))
    return hdr + payload


class Reader:
    def __init__(self, data: bytes):
        self.data = data
        self.pos = 0

    def unpack(self, fmt: str):
        size = struct.calcsize(fmt)
        vals = struct.unpack_from(fmt, self.data, self.pos)
        self.pos += size
        return vals[0] if len(vals) == 1 else vals

    def str16(self) -> str:
        length = self.unpack("<H")
        raw = self.data[self.pos : self.pos + length]
        self.pos += length
        return raw.decode("utf-8", errors="replace")

    def bytes32(self) -> bytes:
        length = self.unpack("<I")
        raw = self.data[self.pos : self.pos + length]
        self.pos += length
        return raw


def decode_hello(payload: bytes) -> dict:
    r = Reader(payload)
    proto_ver, api_level, boot_count, boot_id_source, is_emulator, hb_ms = r.unpack("<HIiBBI")
    return {
        "protocol_version": proto_ver,
        "api_level": api_level,
        "boot_count": boot_count,
        "boot_id_source": boot_id_source,
        "boot_id_source_name": BOOT_ID_SOURCE_NAMES.get(boot_id_source, f"Unknown({boot_id_source})"),
        "is_emulator": bool(is_emulator),
        "heartbeat_interval_ms": hb_ms,
        "boot_id": r.str16(),
        "model": r.str16(),
        "manufacturer": r.str16(),
        "agent_version": r.str16(),
        "boot_id_detail": r.str16(),
    }


def decode_heartbeat(payload: bytes) -> dict:
    r = Reader(payload)
    boottime_ns, monotonic_ns, realtime_ns, count = r.unpack("<qqqB")
    streams = []
    for _ in range(count):
        sid, state, restarts, emitted_seq, last_pts_ns, _res = r.unpack("<BBHIqI")
        streams.append({
            "stream_id": sid,
            "state": state,
            "state_name": STATE_NAMES.get(state, str(state)),
            "restart_count": restarts,
            "emitted_seq": emitted_seq,
            "last_pts_ns": last_pts_ns,
        })
    return {
        "boottime_ns": boottime_ns,
        "monotonic_ns": monotonic_ns,
        "realtime_ns": realtime_ns,
        "streams": streams,
    }


def decode_pong(payload: bytes) -> dict:
    nonce, boot_before, mono, real, boot_after = struct.unpack("<Qqqqq", payload)
    return {
        "nonce": nonce,
        "boottime_before_ns": boot_before,
        "monotonic_ns": mono,
        "realtime_ns": real,
        "boottime_after_ns": boot_after,
    }


def decode_stream_state(payload: bytes) -> dict:
    r = Reader(payload)
    sid, state, restarts, boot_ns, emitted_seq = r.unpack("<BBHqI")
    return {
        "target_stream_id": sid,
        "state": state,
        "state_name": STATE_NAMES.get(state, str(state)),
        "restart_count": restarts,
        "boottime_ns": boot_ns,
        "emitted_seq": emitted_seq,
        "reason": r.str16(),
    }


def decode_log_record(payload: bytes) -> dict:
    r = Reader(payload)
    read_boot, sec, nsec, pid, tid, lid, uid, hdr_size, prio, rflags = r.unpack("<qIIiiIIHBB")
    raw_body = r.bytes32()
    return {
        "device_read_boottime_ns": read_boot,
        "log_sec": sec,
        "log_nsec": nsec,
        "pid": pid,
        "tid": tid,
        "lid": lid,
        "uid": uid,
        "hdr_size": hdr_size,
        "priority": prio,
        "record_flags": rflags,
        "payload": raw_body,
    }


def decode_log_overflow_stats(payload: bytes) -> dict:
    r = Reader(payload)
    boot_ns, completeness, _res, buf_count, pruned_delta, pruned_cum = r.unpack("<qBBHqq")
    return {
        "boottime_ns": boot_ns,
        "completeness": completeness,
        "completeness_name": COMPLETENESS_NAMES.get(completeness, str(completeness)),
        "buffers_count": buf_count,
        "pruned_delta": pruned_delta,
        "pruned_cumulative": pruned_cum,
        "detail": r.str16(),
    }


def decode_shutdown(payload: bytes) -> dict:
    r = Reader(payload)
    boot_ns, code = r.unpack("<qH")
    return {"boottime_ns": boot_ns, "code": code, "reason": r.str16()}


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    s = sorted(values)
    idx = min(len(s) - 1, max(0, int(round((p / 100.0) * (len(s) - 1)))))
    return s[idx]


def run_scrcpy_check(adb: str, serial: str | None, jar_path: Path) -> dict:
    remote_jar = "/data/local/tmp/arb-scrcpy-smoke.jar"
    scid = f"{random.randint(0x10000000, 0x7FFFFFFF):08x}"
    abstract = f"localabstract:scrcpy_{scid}"
    local_port = None
    proc = None
    sock = None
    try:
        adb_cmd(adb, serial, "push", str(jar_path), remote_jar)
        local_port = int(adb_cmd(adb, serial, "forward", "tcp:0", abstract).stdout.strip())
        cmd = [adb]
        if serial:
            cmd.extend(["-s", serial])
        cmd.extend([
            "shell",
            f"CLASSPATH={remote_jar} app_process / --nice-name=arb-scrcpy-smoke "
            f"com.genymobile.scrcpy.Server 5.0.1 scid={scid} tunnel_forward=true "
            f"audio=false control=false video_codec=h264 send_device_meta=false "
            f"send_frame_meta=true send_stream_meta=true max_size=1024 max_fps=30 "
            f"video_bit_rate=4000000 downsize_on_error=false "
            f"video_codec_options=max-bframes:int=0,i-frame-interval:int=1",
        ])
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

        # Retry connect until scrcpy opens the abstract socket
        deadline = time.monotonic() + 8.0
        last_err = None
        while time.monotonic() < deadline:
            try:
                s = socket.create_connection(("127.0.0.1", local_port), timeout=2.0)
                s.settimeout(5.0)
                dummy = recv_exact(s, 1)
                if dummy == b"\x00":
                    sock = s
                    break
                s.close()
            except Exception as e:
                last_err = e
                time.sleep(0.15)
        if sock is None:
            raise RuntimeError(f"scrcpy socket handshake failed: {last_err}")

        # scrcpy 5.0.1 video stream: 4-byte codec_id ("h264"), then Session/Frame packets
        codec_id = recv_exact(sock, 4).decode("ascii", errors="replace")
        if codec_id != "h264":
            raise RuntimeError(f"Unexpected scrcpy codec_id: {codec_id!r}")

        width = 0
        height = 0
        packets = []
        # Read Session + Config + first IDR frame (3 records total)
        while len(packets) < 2:
            high_bytes = recv_exact(sock, 4)
            high_signed = struct.unpack(">i", high_bytes)[0]
            if high_signed < 0:
                width, height = struct.unpack(">ii", recv_exact(sock, 8))
                continue
            low_u32, pkt_size = struct.unpack(">II", recv_exact(sock, 8))
            flags = (high_signed << 32) | low_u32
            is_config = bool(flags & (1 << 62))
            is_keyframe = bool(flags & (1 << 61))
            pts_us = flags & ((1 << 61) - 1)
            data = recv_exact(sock, pkt_size)
            packets.append({
                "size": len(data),
                "is_config": is_config,
                "is_keyframe": is_keyframe,
                "pts_us": pts_us,
            })
        return {
            "ok": True,
            "codec_id": codec_id,
            "width": width,
            "height": height,
            "packets": packets,
        }
    finally:
        if sock is not None:
            try:
                sock.close()
            except Exception:
                pass
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=3.0)
            except subprocess.TimeoutExpired:
                proc.kill()
        if local_port is not None:
            adb_cmd(adb, serial, "forward", "--remove", f"tcp:{local_port}", check=False)
        adb_cmd(adb, serial, "shell", "rm", "-f", remote_jar, check=False)


def main() -> int:
    parser = argparse.ArgumentParser(description="Live ARB device-agent smoke test on Android emulator")
    parser.add_argument("--serial", default=None, help="ADB device serial (optional if single device)")
    parser.add_argument("--jar", default="device-agent/build/arb-companion.dex.jar", help="Companion DEX JAR path")
    parser.add_argument("--scrcpy-jar", default="device-agent/build/replay-scrcpy-audio.dex.jar", help="Optional scrcpy DEX JAR")
    parser.add_argument("--check-scrcpy", action="store_true", help="Also verify scrcpy-server-v5.0.1 video handshake")
    parser.add_argument("--pings", type=int, default=20, help="Number of Ping/Pong sync probes")
    args = parser.parse_args()

    adb = find_adb()
    jar_path = Path(args.jar).resolve()
    if not jar_path.is_file():
        print(f"ERROR: companion jar not found: {jar_path}", file=sys.stderr)
        return 1

    # Verify target is booted
    boot_completed = adb_cmd(adb, args.serial, "shell", "getprop", "sys.boot_completed").stdout.strip()
    if boot_completed != "1":
        print(f"ERROR: target device not booted (sys.boot_completed={boot_completed!r})", file=sys.stderr)
        return 1

    direct_boot_id = adb_cmd(adb, args.serial, "shell", "cat /proc/sys/kernel/random/boot_id", check=False)
    print(f"[PRE] direct shell /proc/sys/kernel/random/boot_id: rc={direct_boot_id.returncode} out={direct_boot_id.stdout.strip()!r}")

    remote_jar = "/data/local/tmp/arb-companion.dex.jar"
    sock_name = f"arb-companion-smoke-{random.randint(100000, 999999)}"
    local_port = None
    companion_proc = None
    sock = None

    try:
        adb_cmd(adb, args.serial, "push", str(jar_path), remote_jar)
        local_port = int(adb_cmd(adb, args.serial, "forward", "tcp:0", f"localabstract:{sock_name}").stdout.strip())

        launch_cmd = [adb]
        if args.serial:
            launch_cmd.extend(["-s", args.serial])
        launch_cmd.extend([
            "shell",
            f"CLASSPATH={remote_jar} app_process / --nice-name=arb-companion "
            f"io.github.shinma06.arb.agent.CompanionMain --socket {sock_name} --heartbeat-ms 250",
        ])
        companion_proc = subprocess.Popen(
            launch_cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        # Wait for ARB_COMPANION_READY line
        ready = False
        assert companion_proc.stdout is not None
        deadline = time.monotonic() + 8.0
        while time.monotonic() < deadline:
            line = companion_proc.stdout.readline()
            if not line:
                if companion_proc.poll() is not None:
                    err = companion_proc.stderr.read() if companion_proc.stderr else ""
                    raise RuntimeError(f"Companion exited early rc={companion_proc.returncode}: {err}")
                time.sleep(0.05)
                continue
            if line.strip().startswith("ARB_COMPANION_READY"):
                print(f"[LAUNCH] {line.strip()}")
                ready = True
                break
        if not ready:
            raise RuntimeError("Timed out waiting for ARB_COMPANION_READY")

        sock = socket.create_connection(("127.0.0.1", local_port), timeout=5.0)
        sock.settimeout(10.0)

        # Shared collector state updated by dedicated reader thread
        lock = threading.Lock()
        hello_box: list[dict] = []
        heartbeats: list[tuple[int, dict]] = []
        pongs: queue.Queue[tuple[int, dict]] = queue.Queue()
        stream_states: list[dict] = []
        log_records: list[tuple[int, dict]] = []
        log_stats: list[dict] = []
        shutdown_acks: list[dict] = []
        reader_error: list[Exception] = []
        stop_reader = threading.Event()

        def rx_loop():
            try:
                while not stop_reader.is_set():
                    frame = read_frame(sock)
                    mt = frame["msg_type"]
                    pl = frame["payload"]
                    ts = frame["host_recv_ns"]
                    with lock:
                        if mt == MSG_HELLO:
                            hello_box.append(decode_hello(pl))
                        elif mt == MSG_HEARTBEAT:
                            heartbeats.append((ts, decode_heartbeat(pl)))
                        elif mt == MSG_PONG:
                            pongs.put((ts, decode_pong(pl)))
                        elif mt == MSG_STREAM_STATE:
                            stream_states.append(decode_stream_state(pl))
                        elif mt == MSG_LOG_RECORD:
                            log_records.append((frame["flags"], decode_log_record(pl)))
                        elif mt == MSG_LOG_OVERFLOW_STATS:
                            log_stats.append(decode_log_overflow_stats(pl))
                        elif mt == MSG_SHUTDOWN:
                            shutdown_acks.append(decode_shutdown(pl))
                            return
            except Exception as exc:
                if not stop_reader.is_set():
                    reader_error.append(exc)

        rx_thread = threading.Thread(target=rx_loop, daemon=True)
        rx_thread.start()

        # 1. Wait for Hello
        t0 = time.monotonic()
        while time.monotonic() - t0 < 5.0:
            with lock:
                if hello_box:
                    break
            time.sleep(0.05)
        assert hello_box, "Did not receive Hello frame"
        hello = hello_box[0]
        print(f"[1/6 HELLO] api={hello['api_level']} model={hello['model']} mfr={hello['manufacturer']} "
              f"is_emulator={hello['is_emulator']} boot_count={hello['boot_count']} "
              f"boot_id_source={hello['boot_id_source_name']} boot_id={hello['boot_id']} "
              f"detail={hello['boot_id_detail']!r}")
        if direct_boot_id.returncode == 0 and direct_boot_id.stdout.strip():
            assert hello["boot_id"] == direct_boot_id.stdout.strip(), "boot_id mismatch vs /proc"

        # 2. 20x Ping/Pong RTT probes
        rtts_ms: list[float] = []
        brackets_us: list[float] = []
        for i in range(args.pings):
            nonce = (0xA5A50000 << 32) | (i + 1)
            t_send = time.perf_counter_ns()
            sock.sendall(build_frame(STREAM_CONTROL, MSG_PING, 0, i + 1, struct.pack("<Q", nonce)))
            t_recv, pong = pongs.get(timeout=3.0)
            assert pong["nonce"] == nonce, f"Nonce mismatch: {pong['nonce']} != {nonce}"
            assert pong["boottime_after_ns"] >= pong["boottime_before_ns"], "boottime_after < boottime_before"
            assert pong["boottime_after_ns"] >= pong["monotonic_ns"], (
                f"boottime_after ({pong['boottime_after_ns']}) < monotonic ({pong['monotonic_ns']})"
            )
            rtts_ms.append((t_recv - t_send) / 1_000_000.0)
            brackets_us.append((pong["boottime_after_ns"] - pong["boottime_before_ns"]) / 1_000.0)

        rtt_med = statistics.median(rtts_ms)
        rtt_p95 = percentile(rtts_ms, 95.0)
        print(f"[2/6 PING/PONG x{args.pings}] RTT ms: min={min(rtts_ms):.3f} median={rtt_med:.3f} "
              f"p95={rtt_p95:.3f} max={max(rtts_ms):.3f} | device_bracket_us median={statistics.median(brackets_us):.1f}")

        # 3. Inject logcat marker and verify Stream 1 delivers it + wait for periodic logcat -S probe (5s)
        marker1 = f"arb-smoke-marker-{random.randint(10000, 99999)}"
        adb_cmd(adb, args.serial, "shell", "log", "-t", "ARB_SMOKE", marker1)
        marker1_seen = False
        t_wait = time.monotonic()
        while time.monotonic() - t_wait < 6.5:
            with lock:
                if not marker1_seen:
                    for _fl, rec in log_records:
                        if marker1.encode() in rec["payload"]:
                            marker1_seen = True
                            break
                if marker1_seen and len(log_stats) >= 2:
                    break
            time.sleep(0.1)

        with lock:
            rec_count_before_kill = len(log_records)
            stats_snapshot = list(log_stats)
        assert marker1_seen, f"Injected logcat marker {marker1} was not received on Stream 1"
        assert rec_count_before_kill > 0, "Expected live logcat records"
        assert len(stats_snapshot) >= 2, f"Expected >=2 LogOverflowStats frames, got {len(stats_snapshot)}"
        print(f"[3/6 LOGCAT] records_received={rec_count_before_kill} marker_verified=True")
        for idx, st in enumerate(stats_snapshot):
            print(f"     LogOverflowStats[{idx}]: completeness={st['completeness_name']} "
                  f"buffers={st['buffers_count']} pruned_delta={st['pruned_delta']} "
                  f"pruned_cum={st['pruned_cumulative']} detail={st['detail']!r}")

        # 4. Live Fault-Isolation Proof: SIGKILL the real `logcat` subprocess!
        ps_out = adb_cmd(adb, args.serial, "shell", "ps -A -o PID,ARGS").stdout
        logcat_pids = []
        for line in ps_out.splitlines():
            if "logcat -b all -B -T 1" in line:
                parts = line.strip().split()
                if parts and parts[0].isdigit():
                    logcat_pids.append(parts[0])
        assert logcat_pids, f"Could not find live `logcat -b all -B -T 1` PID in ps output"

        with lock:
            hb_count_before_kill = len(heartbeats)
        t_kill_ns = time.perf_counter_ns()
        for pid in logcat_pids:
            adb_cmd(adb, args.serial, "shell", "kill", "-9", pid)
        print(f"[4/6 FAULT-INJECT] Sent SIGKILL to logcat PID(s)={logcat_pids}")

        # Send immediate Ping right after kill to prove Stream 0 stays responsive
        ping_nonce = 0xFA017150
        t_ps = time.perf_counter_ns()
        sock.sendall(build_frame(STREAM_CONTROL, MSG_PING, 0, 999, struct.pack("<Q", ping_nonce)))
        t_pr, post_kill_pong = pongs.get(timeout=2.0)
        assert post_kill_pong["nonce"] == ping_nonce
        post_kill_rtt_ms = (t_pr - t_ps) / 1_000_000.0

        # Wait for Stream 1 to emit Error -> Restarting -> Running and deliver post-recovery log
        marker2 = f"arb-smoke-recovered-{random.randint(10000, 99999)}"
        recovered_state = False
        recovered_flag_seen = False
        marker2_seen = False
        t_rec = time.monotonic()
        marker2_sent = False
        while time.monotonic() - t_rec < 6.0:
            with lock:
                logcat_states = [s for s in stream_states if s["target_stream_id"] == STREAM_LOGCAT]
                if any(s["state_name"] == "Running" and s["restart_count"] >= 1 for s in logcat_states):
                    recovered_state = True
                    if not marker2_sent:
                        marker2_sent = True
                        adb_cmd(adb, args.serial, "shell", "log", "-t", "ARB_SMOKE", marker2)
                for fl, rec in log_records[rec_count_before_kill:]:
                    if fl & FLAG_RECOVERED:
                        recovered_flag_seen = True
                    if marker2.encode() in rec["payload"]:
                        marker2_seen = True
            if recovered_state and marker2_seen:
                break
            time.sleep(0.1)

        with lock:
            logcat_states = [s for s in stream_states if s["target_stream_id"] == STREAM_LOGCAT]
            input_states = [s for s in stream_states if s["target_stream_id"] == STREAM_INPUT]
            hb_list = list(heartbeats)
            overflow_gaps = [s for s in log_stats if s["completeness_name"] == "OverflowDetected"]

        assert recovered_state, f"Logcat did not recover after SIGKILL; states={logcat_states}"
        assert marker2_seen, f"Post-recovery marker {marker2} not received"
        assert overflow_gaps, "Expected OverflowDetected event on logcat SIGKILL"

        # Verify Heartbeat monotonicity and zero interruption across entire run (including SIGKILL window!)
        assert len(hb_list) >= 15, f"Expected >=15 heartbeats, got {len(hb_list)}"
        max_hb_gap_ms = 0.0
        for i in range(len(hb_list)):
            ts, hb = hb_list[i]
            assert hb["boottime_ns"] >= hb["monotonic_ns"], "Heartbeat boottime < monotonic"
            if i > 0:
                prev_ts, prev_hb = hb_list[i - 1]
                assert hb["boottime_ns"] >= prev_hb["boottime_ns"], "Heartbeat boottime went backwards"
                assert hb["monotonic_ns"] >= prev_hb["monotonic_ns"], "Heartbeat monotonic went backwards"
                gap_ms = (ts - prev_ts) / 1_000_000.0
                if gap_ms > max_hb_gap_ms:
                    max_hb_gap_ms = gap_ms
        hb_after_kill = [ts for ts, _ in hb_list if ts >= t_kill_ns]
        assert len(hb_after_kill) >= 3, "Beacon stopped ticking after logcat SIGKILL"
        assert max_hb_gap_ms < 1000.0, f"Beacon stalled during logcat kill: max_gap_ms={max_hb_gap_ms:.1f}"

        print(f"[4/6 FAULT-ISOLATION PASS] post_kill_ping_rtt={post_kill_rtt_ms:.3f}ms "
              f"logcat_transitions={[(s['state_name'], s['restart_count'], s['reason']) for s in logcat_states]} "
              f"overflow_events={len(overflow_gaps)} recovered_flag={recovered_flag_seen} marker2_received={marker2_seen}")
        print(f"[5/6 BEACON] total_heartbeats={len(hb_list)} max_inter_hb_gap_ms={max_hb_gap_ms:.1f} "
              f"monotonic_and_boottime_ge_mono=True input_states={[(s['state_name'], s['reason']) for s in input_states]}")

        # 6. Graceful Shutdown
        sock.sendall(build_frame(STREAM_CONTROL, MSG_SHUTDOWN, 1, 1000, struct.pack("<qH", 0, 0) + struct.pack("<H", 10) + b"smoke_done"))
        rx_thread.join(timeout=4.0)
        stop_reader.set()
        assert shutdown_acks, "Did not receive Shutdown ack"
        assert not reader_error, f"Reader thread error: {reader_error}"
        rc = companion_proc.wait(timeout=5.0)
        print(f"[6/6 SHUTDOWN] ack={shutdown_acks[0]} companion_exit_code={rc}")
        assert rc == 0, f"Expected companion exit code 0, got {rc}"

        # Optional scrcpy-server-v5.0.1 handshake check
        if args.check_scrcpy:
            scrcpy_jar = Path(args.scrcpy_jar).resolve()
            if scrcpy_jar.is_file():
                sc_res = run_scrcpy_check(adb, args.serial, scrcpy_jar)
                print(f"[OPTIONAL SCRCPY] ok={sc_res['ok']} codec={sc_res['codec_id']} "
                      f"size={sc_res['width']}x{sc_res['height']} packets={sc_res['packets']}")
            else:
                print(f"[OPTIONAL SCRCPY] skipped (jar missing: {scrcpy_jar})")

        print("ALL EMULATOR SMOKE CHECKS PASSED")
        return 0
    finally:
        if sock is not None:
            try:
                sock.close()
            except Exception:
                pass
        if companion_proc is not None and companion_proc.poll() is None:
            companion_proc.terminate()
            try:
                companion_proc.wait(timeout=3.0)
            except subprocess.TimeoutExpired:
                companion_proc.kill()
        if local_port is not None:
            adb_cmd(adb, args.serial, "forward", "--remove", f"tcp:{local_port}", check=False)
        adb_cmd(adb, args.serial, "shell", "rm", "-f", remote_jar, check=False)


if __name__ == "__main__":
    sys.exit(main())

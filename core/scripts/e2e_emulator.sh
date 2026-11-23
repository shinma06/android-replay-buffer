#!/usr/bin/env bash
set -euo pipefail

# Live headless Android Emulator E2E verification for ARB v2 (Gate G2).
# Exercises the real device-agent -> arbd -> arb-cli pipeline end-to-end on
# the `arb_api37_arm64` AVD and guarantees full cleanup on exit or error.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
AVD_NAME="${ARB_E2E_AVD:-arb_api37_arm64}"

ANDROID_SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
ADB="${ANDROID_SDK}/platform-tools/adb"
EMULATOR="${ANDROID_SDK}/emulator/emulator"
FFPROBE="$(command -v ffprobe || true)"
FFMPEG="$(command -v ffmpeg || true)"
ENABLE_AUDIO="${ARB_E2E_ENABLE_AUDIO:-1}"
PLAYER_MATRIX_SCRIPT="${REPO_ROOT}/core/scripts/devin_verify_player_matrix.py"

for bin in "${ADB}" "${EMULATOR}" python3 cargo; do
    if ! command -v "${bin}" >/dev/null 2>&1 && [[ ! -x "${bin}" ]]; then
        echo "ERROR: required tool not found: ${bin}" >&2
        exit 1
    fi
done
if [[ -z "${FFPROBE}" ]]; then
    echo "ERROR: ffprobe is required for independent MP4 verification" >&2
    exit 1
fi

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/arb-e2e-emu.XXXXXX")"
DATA_DIR="${WORK_DIR}/data"
CACHE_DIR="${WORK_DIR}/cache"
RUNTIME_DIR="${WORK_DIR}/run"
OUT_DIR="${WORK_DIR}/out"
EMU_LOG="${WORK_DIR}/emulator.log"
mkdir -p "${DATA_DIR}" "${CACHE_DIR}" "${RUNTIME_DIR}" "${OUT_DIR}"

PROFILE="${ARB_E2E_PROFILE:-release}"
if [[ "${PROFILE}" == "release" ]]; then
    CARGO_PROFILE_ARGS=(--release)
    TARGET_SUBDIR="release"
else
    CARGO_PROFILE_ARGS=()
    TARGET_SUBDIR="debug"
fi

ARBD_BIN="${REPO_ROOT}/core/target/${TARGET_SUBDIR}/arbd"
ARB_CLI_BIN="${REPO_ROOT}/core/target/${TARGET_SUBDIR}/arb-cli"
COMPANION_JAR="${REPO_ROOT}/device-agent/build/arb-companion.dex.jar"
SCRCPY_JAR="${REPO_ROOT}/device-agent/build/replay-scrcpy-audio.dex.jar"

EMU_PID=""
EMU_SERIAL=""
AUDIO_PLAY_PID=""
CLEANED_UP=0

run_cli() {
    "${ARB_CLI_BIN}" \
        --data-dir "${DATA_DIR}" \
        --cache-dir "${CACHE_DIR}" \
        --runtime-dir "${RUNTIME_DIR}" \
        "$@"
}

cleanup() {
    local exit_code="${1:-$?}"
    if [[ "${CLEANED_UP}" -eq 1 ]]; then
        return
    fi
    CLEANED_UP=1
    set +e

    echo ""
    echo "==> [Cleanup] Stopping arbd, shutting down script-launched emulator, cleaning temp files..."

    if [[ -n "${AUDIO_PLAY_PID}" ]]; then
        kill "${AUDIO_PLAY_PID}" 2>/dev/null || true
    fi
    if [[ -x "${ARB_CLI_BIN}" ]]; then
        run_cli capture stop >/dev/null 2>&1 || true
        run_cli daemon stop >/dev/null 2>&1 || true
    fi

    # Wait briefly for arbd to exit cleanly, then ensure no orphan remains for our runtime_dir.
    # R3 S18: SingleInstanceLock writes "<pid>\n", not JSON; parse digits and escalate to SIGKILL.
    sleep 1
    if [[ -f "${RUNTIME_DIR}/arbd.lock" ]]; then
        local lock_pid
        lock_pid="$(tr -dc '0-9' < "${RUNTIME_DIR}/arbd.lock" 2>/dev/null || true)"
        if [[ -n "${lock_pid}" ]] && kill -0 "${lock_pid}" 2>/dev/null; then
            kill "${lock_pid}" 2>/dev/null || true
            for _ in $(seq 1 10); do
                if ! kill -0 "${lock_pid}" 2>/dev/null; then
                    break
                fi
                sleep 0.2
            done
            if kill -0 "${lock_pid}" 2>/dev/null; then
                kill -9 "${lock_pid}" 2>/dev/null || true
            fi
        fi
    fi

    # R3 M9: NEVER kill arbitrary pre-existing emulators — only act on the specific
    # EMU_SERIAL and EMU_PID started by this script.
    if [[ -n "${EMU_SERIAL}" ]]; then
        "${ADB}" -s "${EMU_SERIAL}" shell 'pkill -f arb-e2e-tone.wav 2>/dev/null || true; rm -f /data/local/tmp/arb-* /data/local/tmp/replay-scrcpy-*' >/dev/null 2>&1 || true
        "${ADB}" -s "${EMU_SERIAL}" forward --remove-all >/dev/null 2>&1 || true
        "${ADB}" -s "${EMU_SERIAL}" emu kill >/dev/null 2>&1 || true
    fi

    if [[ -n "${EMU_PID}" ]]; then
        if [[ -z "${EMU_SERIAL}" ]] && kill -0 "${EMU_PID}" 2>/dev/null; then
            kill "${EMU_PID}" 2>/dev/null || true
        fi
        for _ in $(seq 1 30); do
            if ! kill -0 "${EMU_PID}" 2>/dev/null; then
                break
            fi
            sleep 1
        done
        if kill -0 "${EMU_PID}" 2>/dev/null; then
            kill -9 "${EMU_PID}" 2>/dev/null || true
        fi
        wait "${EMU_PID}" 2>/dev/null || true
    fi

    # Wait until our specific EMU_SERIAL (if launched) disappears from `adb devices`.
    if [[ -n "${EMU_SERIAL}" ]]; then
        for _ in $(seq 1 20); do
            local still_attached
            still_attached="$("${ADB}" devices | awk -v target="${EMU_SERIAL}" 'NR>1 && $1 == target {print $1}')"
            if [[ -z "${still_attached}" ]]; then
                break
            fi
            sleep 1
        done

        local final_target
        final_target="$("${ADB}" devices | awk -v target="${EMU_SERIAL}" 'NR>1 && $1 == target {print $0}')"
        if [[ -n "${final_target}" ]]; then
            echo "WARN: script-launched emulator ${EMU_SERIAL} still attached after cleanup:" >&2
            echo "${final_target}" >&2
            if [[ "${exit_code}" -eq 0 ]]; then
                exit_code=1
            fi
        else
            echo "==> [Cleanup] Verified: ${EMU_SERIAL} is detached."
        fi
    fi

    # R3 S18: preserve emulator.log and arbd.log on failure so diagnostics are not lost.
    if [[ "${exit_code}" -eq 0 ]]; then
        rm -rf "${WORK_DIR}"
    else
        echo "WARN: E2E failed (exit=${exit_code}); preserving logs under ${WORK_DIR}" >&2
        if [[ -f "${EMU_LOG}" ]]; then
            echo "      emulator log: ${EMU_LOG}" >&2
        fi
        if [[ -f "${RUNTIME_DIR}/arbd.log" ]]; then
            echo "      arbd log:     ${RUNTIME_DIR}/arbd.log" >&2
        fi
    fi
    exit "${exit_code}"
}

trap 'cleanup 130' INT
trap 'cleanup 143' TERM
trap 'cleanup' EXIT

echo "==> [1/9] Building device-agent (arb-companion + scrcpy-audio)..."
(cd "${REPO_ROOT}" && ./device-agent/build.sh --with-scrcpy-audio)
test -f "${COMPANION_JAR}"
test -f "${SCRCPY_JAR}"

echo "==> [2/9] Building arbd and arb-cli (${PROFILE})..."
cargo build --manifest-path "${REPO_ROOT}/core/Cargo.toml" "${CARGO_PROFILE_ARGS[@]}" --bin arbd --bin arb-cli
test -x "${ARBD_BIN}"
test -x "${ARB_CLI_BIN}"

echo "==> [3/9] Booting headless Android Emulator (${AVD_NAME})..."
EXISTING_DEVICES="$("${ADB}" devices | awk 'NR>1 && NF>=2 {print $1}')"
if [[ -n "${EXISTING_DEVICES}" ]]; then
    echo "ERROR: adb already has attached devices before E2E start: ${EXISTING_DEVICES}" >&2
    # R3 M9: skip cleanup kills so pre-existing developer emulators/devices are untouched.
    CLEANED_UP=1
    rm -rf "${WORK_DIR}"
    exit 1
fi

EMU_AUDIO_ARGS=()
if [[ "${ENABLE_AUDIO}" == "0" ]]; then
    EMU_AUDIO_ARGS=(-no-audio)
fi
"${EMULATOR}" \
    -avd "${AVD_NAME}" \
    -no-window \
    ${EMU_AUDIO_ARGS[@]+"${EMU_AUDIO_ARGS[@]}"} \
    -gpu host \
    -no-boot-anim \
    -no-snapshot-save \
    >"${EMU_LOG}" 2>&1 &
EMU_PID=$!

echo "    Waiting for emulator ADB device & sys.boot_completed=1 (pid=${EMU_PID})..."
BOOT_READY=0
for i in $(seq 1 120); do
    if ! kill -0 "${EMU_PID}" 2>/dev/null; then
        echo "ERROR: emulator process exited prematurely. Last 40 log lines:" >&2
        tail -n 40 "${EMU_LOG}" >&2 || true
        exit 1
    fi
    EMU_SERIAL="$("${ADB}" devices | awk 'NR>1 && $2 == "device" && $1 ~ /^emulator-/ {print $1; exit}')"
    if [[ -n "${EMU_SERIAL}" ]]; then
        BOOT_VAL="$("${ADB}" -s "${EMU_SERIAL}" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r\n' || true)"
        if [[ "${BOOT_VAL}" == "1" ]]; then
            BOOT_READY=1
            echo "    Booted ${EMU_SERIAL} in ${i}s."
            break
        fi
    fi
    sleep 1
done

if [[ "${BOOT_READY}" -ne 1 ]]; then
    echo "ERROR: emulator did not complete boot within 120s." >&2
    tail -n 40 "${EMU_LOG}" >&2 || true
    exit 1
fi

# Wake & unlock screen so SurfaceFlinger continuously renders launcher/settings.
"${ADB}" -s "${EMU_SERIAL}" shell input keyevent 82 >/dev/null 2>&1 || true
"${ADB}" -s "${EMU_SERIAL}" shell wm dismiss-keyguard >/dev/null 2>&1 || true
"${ADB}" -s "${EMU_SERIAL}" shell svc power stayon true >/dev/null 2>&1 || true
sleep 2

echo "==> [4/9] Starting arbd via arb-cli --auto-start and verifying device discovery..."
STATUS_JSON="$(run_cli --auto-start --json status)"
echo "    status: ${STATUS_JSON}"
DEVICES_JSON="$(run_cli --json devices)"
echo "    devices: ${DEVICES_JSON}"
python3 - "${DEVICES_JSON}" "${EMU_SERIAL}" <<'PY'
import json, sys
data = json.loads(sys.argv[1])
serial = sys.argv[2]
found = any(d.get("serial") == serial and d.get("state") == "device" for d in data.get("devices", []))
assert found, f"expected {serial} in device.list: {data}"
PY

echo "==> [5/9] Starting live capture (audio=${ENABLE_AUDIO}) and driving ~8s of real UI/audio/log/input activity..."
CAPTURE_AUDIO_ARGS=()
if [[ "${ENABLE_AUDIO}" == "0" ]]; then
    CAPTURE_AUDIO_ARGS=(--no-audio)
elif [[ -n "${FFMPEG}" ]]; then
    # Synthesize a real 12s 440Hz stereo 48kHz WAV and play it via Android's built-in
    # stagefright AudioTrack client so scrcpy-server playback duplication captures live AAC-LC.
    "${FFMPEG}" -y -v error -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=12" \
        -ac 2 -c:a pcm_s16le "${WORK_DIR}/arb-e2e-tone.wav"
    "${ADB}" -s "${EMU_SERIAL}" push "${WORK_DIR}/arb-e2e-tone.wav" /data/local/tmp/arb-e2e-tone.wav >/dev/null
    "${ADB}" -s "${EMU_SERIAL}" shell cmd media_session volume --stream 3 --set 10 >/dev/null 2>&1 || true
fi

START_JSON="$(run_cli --json capture start \
    --serial "${EMU_SERIAL}" \
    --companion-jar "${COMPANION_JAR}" \
    --scrcpy-jar "${SCRCPY_JAR}" \
    ${CAPTURE_AUDIO_ARGS[@]+"${CAPTURE_AUDIO_ARGS[@]}"} \
    --max-size 1024 \
    --max-fps 30)"
echo "    capture.start: ${START_JSON}"

# Wait until companion + video/audio handshakes complete and frames start flowing.
sleep 2
if [[ "${ENABLE_AUDIO}" != "0" ]] && [[ -n "${FFMPEG}" ]]; then
    "${ADB}" -s "${EMU_SERIAL}" shell '/system_ext/bin/stagefright -a -o /data/local/tmp/arb-e2e-tone.wav' >/dev/null 2>&1 &
    AUDIO_PLAY_PID=$!
fi
"${ADB}" -s "${EMU_SERIAL}" shell am start -a android.settings.SETTINGS >/dev/null 2>&1 || true
for step in 1 2 3 4 5 6; do
    "${ADB}" -s "${EMU_SERIAL}" shell log -t ARB_E2E "live-e2e-marker-${step}" >/dev/null 2>&1 || true
    if (( step % 2 == 1 )); then
        "${ADB}" -s "${EMU_SERIAL}" shell input swipe 500 1400 500 500 350 >/dev/null 2>&1 || true
    else
        "${ADB}" -s "${EMU_SERIAL}" shell input swipe 500 500 500 1400 350 >/dev/null 2>&1 || true
    fi
    "${ADB}" -s "${EMU_SERIAL}" shell input tap 500 800 >/dev/null 2>&1 || true
    sleep 0.8
done

LIVE_STATUS_JSON="$(run_cli --json status)"
echo "    live status: ${LIVE_STATUS_JSON}"

echo "==> [6/9] Triggering live save (30s window) and stopping capture..."
SAVE_JSON="$(run_cli --json save --seconds 30)"
echo "    save: ${SAVE_JSON}"
run_cli --json capture stop >/dev/null

BUNDLE_ID="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); assert d["job"]["state"]=="completed", d; print(d["bundle_id"])' "${SAVE_JSON}")"
BUNDLE_PATH="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print(d["bundle_path"])' "${SAVE_JSON}")"
echo "    saved bundle_id=${BUNDLE_ID} path=${BUNDLE_PATH}"

echo "==> [7/9] Inspecting bundle and running real IPC + SHM scrub-bench..."
INSPECT_JSON="$(run_cli --json inspect "${BUNDLE_ID}")"
echo "    inspect: ${INSPECT_JSON}"

FRAME_COUNT="$(python3 - "${INSPECT_JSON}" <<'PY'
import json, sys
d = json.loads(sys.argv[1])
s = d["summary"]
fc = s["frame_count"]
kc = s["keyframe_count"]
lc = s["log_main_count"]
assert fc >= 15, f"expected >= 15 live captured frames, got {fc} ({s})"
assert kc >= 2, f"expected >= 2 keyframes across multiple GOPs, got {kc} ({s})"
assert lc > 0, f"expected > 0 live logcat entries, got {lc} ({s})"
print(fc)
PY
)"

SCRUB_JSON="$(run_cli --json scrub-bench "${BUNDLE_ID}")"
echo "    scrub-bench: ${SCRUB_JSON}"
python3 - "${SCRUB_JSON}" <<'PY'
import json, sys
d = json.loads(sys.argv[1])
assert d["filmstrip"]["count"] >= 2, d
assert d["scrub"]["shm_is_approximate"] is True, d
assert d["seek_exact"]["shm_is_approximate"] is False, d
assert d["seek_exact"]["shm_frame_index"] == d["target_frame"], d
assert d["step_backward"]["cache_hits"] >= 2, d
PY

echo "==> [8/9] Performing mid-GOP trim and verifying with ffprobe (0 edts/elst, H.264, exact frame count)..."
TRIM_RANGE="$(python3 - "${BUNDLE_PATH}/timeline.sqlite" <<'PY'
import sqlite3, sys
conn = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
rows = conn.execute("SELECT frame_index, is_keyframe FROM frames ORDER BY frame_index ASC").fetchall()
marker_count = conn.execute("SELECT COUNT(*) FROM logs WHERE tag = 'ARB_E2E'").fetchone()[0]
conn.close()
kfs = [r[0] for r in rows if r[1]]
assert len(kfs) >= 2, f"need >= 2 keyframes for mid-GOP trim, got {kfs}"
assert marker_count >= 1, f"expected >= 1 injected ARB_E2E logcat marker, got {marker_count}"
# Pick a genuine non-keyframe inside GOP 0 and end after keyframe 1.
in_frame = kfs[0] + max(1, (kfs[1] - kfs[0]) // 2)
assert in_frame not in kfs, f"in_frame {in_frame} unexpectedly a keyframe"
out_frame = min(len(rows) - 1, kfs[1] + max(2, (len(rows) - 1 - kfs[1]) // 2))
assert out_frame > kfs[1], f"out_frame {out_frame} must cross keyframe {kfs[1]}"
print(f"{in_frame} {out_frame} {marker_count}")
PY
)"
IN_FRAME="$(echo "${TRIM_RANGE}" | awk '{print $1}')"
OUT_FRAME="$(echo "${TRIM_RANGE}" | awk '{print $2}')"
MARKER_COUNT="$(echo "${TRIM_RANGE}" | awk '{print $3}')"
EXPECTED_TRIM_FRAMES=$((OUT_FRAME - IN_FRAME + 1))
echo "    live ARB_E2E logcat markers captured: ${MARKER_COUNT}"
echo "    mid-GOP trim range: [${IN_FRAME}..=${OUT_FRAME}] (expected ${EXPECTED_TRIM_FRAMES} frames)"

TRIM_START_NS="$(python3 -c 'import time; print(time.monotonic_ns())')"
TRIM_JSON="$(run_cli --json trim "${BUNDLE_ID}" --in-frame "${IN_FRAME}" --out-frame "${OUT_FRAME}" --dest "${OUT_DIR}")"
TRIM_END_NS="$(python3 -c 'import time; print(time.monotonic_ns())')"
TRIM_ELAPSED_MS="$(python3 -c "print(f'{(${TRIM_END_NS}-${TRIM_START_NS})/1e6:.1f}')")"
echo "    trim_elapsed_ms: ${TRIM_ELAPSED_MS} ms (range [${IN_FRAME}..=${OUT_FRAME}], ${EXPECTED_TRIM_FRAMES} frames)"
echo "    trim: ${TRIM_JSON}"
TRIMMED_PATH="$(python3 - "${TRIM_JSON}" "${EXPECTED_TRIM_FRAMES}" <<'PY'
import json, sys
d = json.loads(sys.argv[1])
expected = int(sys.argv[2])
rep = d["trim_report"]
assert rep["output_frame_count"] == expected, d
assert rep["edts_box_count"] == 0, d
assert rep["elst_box_count"] == 0, d
print(d["bundle_path"])
PY
)"

if [[ "${ENABLE_AUDIO}" != "0" ]] && [[ -n "${FFMPEG}" ]]; then
    python3 - "${BUNDLE_PATH}" "${TRIMMED_PATH}" <<'PY'
import json, pathlib, sys
for label, p in [("saved", pathlib.Path(sys.argv[1])), ("trimmed", pathlib.Path(sys.argv[2]))]:
    m = json.loads((p / "manifest.json").read_text())
    aud = m["streams"]["audio"]
    m4a = p / "audio.m4a"
    assert m4a.is_file() and m4a.stat().st_size > 0, f"{label} bundle missing audio.m4a: {aud}"
    assert aud["status"] == "present", f"{label} audio status expected present: {aud}"
    print(f"    {label} audio check PASSED: bytes={m4a.stat().st_size}, av_offset_ms={aud['av_offset_ms']}, audio_lead_in_offset_ns={aud['audio_lead_in_offset_ns']}")
PY
fi

if [[ -n "${FFMPEG}" ]] && [[ -f "${PLAYER_MATRIX_SCRIPT}" ]]; then
    echo "    Running devin_verify_player_matrix.py on SAVED bundle (${FRAME_COUNT} frames)..."
    python3 "${PLAYER_MATRIX_SCRIPT}" \
        --mp4 "${BUNDLE_PATH}/video.mp4" \
        --expected-frames "${FRAME_COUNT}" \
        --out-dir "${OUT_DIR}/player-matrix-saved"
    echo "    Running devin_verify_player_matrix.py on TRIMMED bundle (${EXPECTED_TRIM_FRAMES} frames)..."
    python3 "${PLAYER_MATRIX_SCRIPT}" \
        --mp4 "${TRIMMED_PATH}/video.mp4" \
        --expected-frames "${EXPECTED_TRIM_FRAMES}" \
        --out-dir "${OUT_DIR}/player-matrix-trimmed"
fi

TRIMMED_MP4="${TRIMMED_PATH}/video.mp4"
FFPROBE_JSON="$("${FFPROBE}" -v error -select_streams v:0 -count_frames -show_entries stream=codec_name,width,height,nb_frames,nb_read_frames -show_format -of json "${TRIMMED_MP4}")"
echo "    ffprobe: ${FFPROBE_JSON}"

python3 - "${FFPROBE_JSON}" "${TRIMMED_MP4}" "${EXPECTED_TRIM_FRAMES}" <<'PY'
import json, struct, sys

probe = json.loads(sys.argv[1])
mp4_path = sys.argv[2]
expected = int(sys.argv[3])

streams = probe.get("streams", [])
assert len(streams) == 1, f"expected 1 video stream: {probe}"
st = streams[0]
assert st.get("codec_name") == "h264", f"expected h264 codec: {st}"
nb_frames = int(st.get("nb_read_frames") or st.get("nb_frames") or 0)
assert nb_frames == expected, f"ffprobe frame count mismatch: got {nb_frames}, expected {expected}"
assert int(st.get("width", 0)) > 0 and int(st.get("height", 0)) > 0, st

# Independent recursive MP4 box scan confirming 0 edts and 0 elst boxes.
CONTAINERS = {b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"udta"}
data = open(mp4_path, "rb").read()
counts = {b"edts": 0, b"elst": 0, b"moov": 0, b"mdat": 0}
order = []

def walk(start, end, depth=0):
    pos = start
    while pos + 8 <= end:
        size, fourcc = struct.unpack(">I4s", data[pos:pos+8])
        hdr = 8
        if size == 1:
            size = struct.unpack(">Q", data[pos+8:pos+16])[0]
            hdr = 16
        elif size == 0:
            size = end - pos
        if size < hdr or pos + size > end:
            break
        if depth == 0:
            order.append(fourcc.decode("latin1"))
        if fourcc in counts:
            counts[fourcc] += 1
        if fourcc in CONTAINERS:
            walk(pos + hdr, pos + size, depth + 1)
        pos += size

walk(0, len(data), 0)
assert counts[b"edts"] == 0, f"expected 0 edts boxes, found {counts}"
assert counts[b"elst"] == 0, f"expected 0 elst boxes, found {counts}"
assert order.index("moov") < order.index("mdat"), f"expected faststart moov before mdat: {order}"
print(f"    MP4 box check PASSED: top_boxes={order}, edts={counts[b'edts']}, elst={counts[b'elst']}, ffprobe_frames={nb_frames}")
PY

echo "==> [9/9] Exporting exact PNG frame and validating header, dimensions, and tEXt provenance..."
PNG_OUT="${OUT_DIR}/live_frame_${IN_FRAME}.png"
PNG_JSON="$(run_cli --json png "${BUNDLE_ID}" --frame "${IN_FRAME}" --out "${PNG_OUT}")"
echo "    png: ${PNG_JSON}"

python3 - "${PNG_OUT}" "${IN_FRAME}" <<'PY'
import struct, sys

png_path = sys.argv[1]
expected_frame = str(sys.argv[2])
raw = open(png_path, "rb").read()
assert raw[:8] == b"\x89PNG\r\n\x1a\n", "invalid PNG signature"

pos = 8
width = height = 0
texts = {}
while pos + 8 <= len(raw):
    length, ctype = struct.unpack(">I4s", raw[pos:pos+8])
    chunk = raw[pos+8:pos+8+length]
    pos += 12 + length
    if ctype == b"IHDR":
        width, height = struct.unpack(">II", chunk[:8])
    elif ctype == b"tEXt" and b"\x00" in chunk:
        k, v = chunk.split(b"\x00", 1)
        texts[k.decode("latin1")] = v.decode("latin1")
    elif ctype == b"IEND":
        break

assert width > 0 and height > 0, f"invalid PNG dimensions {width}x{height}"
assert texts.get("arb:frame_index") == expected_frame, f"missing/mismatched arb:frame_index: {texts}"
print(f"    PNG check PASSED: {width}x{height}, bytes={len(raw)}, tEXt_keys={sorted(texts.keys())}")
PY

echo ""
echo "================================================================="
echo "LIVE HEADLESS EMULATOR E2E PASSED (AVD=${AVD_NAME}, frames=${FRAME_COUNT})"
echo "================================================================="

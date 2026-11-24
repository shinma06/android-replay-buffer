#!/usr/bin/env bash
set -euo pipefail

# Deterministic build & host unit-test script for ARB device-agent.
# Produces:
#   device-agent/build/arb-companion.dex.jar       (always)
#   device-agent/build/replay-scrcpy-audio.dex.jar (when --with-scrcpy-audio is passed)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BUILD_DIR="${SCRIPT_DIR}/build"

ANDROID_SDK="${ANDROID_HOME:-${HOME}/Library/Android/sdk}"
MIN_API=24
WITH_SCRCPY_AUDIO=0
SKIP_TESTS=0

for arg in "$@"; do
    case "${arg}" in
        --with-scrcpy-audio) WITH_SCRCPY_AUDIO=1 ;;
        --skip-tests) SKIP_TESTS=1 ;;
        -h|--help)
            echo "Usage: $0 [--with-scrcpy-audio] [--skip-tests]"
            exit 0
            ;;
        *)
            echo "Unknown option: ${arg}" >&2
            exit 2
            ;;
    esac
done

# Locate android.jar (prefer android-37.0, fallback to android-37.2 or highest available)
ANDROID_JAR=""
for candidate in \
    "${ANDROID_SDK}/platforms/android-37.0/android.jar" \
    "${ANDROID_SDK}/platforms/android-37.2/android.jar"; do
    if [[ -f "${candidate}" ]]; then
        ANDROID_JAR="${candidate}"
        break
    fi
done
if [[ -z "${ANDROID_JAR}" ]]; then
    ANDROID_JAR="$(find "${ANDROID_SDK}/platforms" -maxdepth 2 -name android.jar 2>/dev/null | sort -V | tail -n 1 || true)"
fi
if [[ -z "${ANDROID_JAR}" || ! -f "${ANDROID_JAR}" ]]; then
    echo "ERROR: android.jar not found under ${ANDROID_SDK}/platforms" >&2
    exit 1
fi

# Locate d8 (prefer 37.0.0, fallback to highest build-tools)
D8_BIN=""
for candidate in \
    "${ANDROID_SDK}/build-tools/37.0.0/d8" \
    "${ANDROID_SDK}/build-tools/36.0.0/d8"; do
    if [[ -x "${candidate}" ]]; then
        D8_BIN="${candidate}"
        break
    fi
done
if [[ -z "${D8_BIN}" ]]; then
    D8_BIN="$(find "${ANDROID_SDK}/build-tools" -maxdepth 2 -name d8 -perm +111 2>/dev/null | sort -V | tail -n 1 || true)"
fi
if [[ -z "${D8_BIN}" || ! -x "${D8_BIN}" ]]; then
    echo "ERROR: d8 not found under ${ANDROID_SDK}/build-tools" >&2
    exit 1
fi

echo "==> Android SDK: ${ANDROID_SDK}"
echo "==> android.jar: ${ANDROID_JAR}"
echo "==> d8:          ${D8_BIN}"

rm -rf "${BUILD_DIR}"
mkdir -p "${BUILD_DIR}/companion/classes" "${BUILD_DIR}/companion/dex" "${BUILD_DIR}/host-test/classes"

# 1. Host pure-Java unit tests (zero Android runtime deps)
if [[ "${SKIP_TESTS}" -eq 0 ]]; then
    echo "==> [1/3] Compiling and running pure-Java host unit tests (ProtocolTest + Aos9CompatTest)"
    PURE_SOURCES=(
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/Protocol.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/WireFrame.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/PayloadCodec.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/BootIdResolver.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/LoggerEntryParser.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/LogcatStatsParser.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/LogcatWorker.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/GeteventParser.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/ClockSource.java"
        "${SCRIPT_DIR}/companion/src/io/github/shinma06/arb/agent/StreamHealthTracker.java"
        "${SCRIPT_DIR}/companion/test/io/github/shinma06/arb/agent/ProtocolTest.java"
        "${SCRIPT_DIR}/companion/test/io/github/shinma06/arb/agent/Aos9CompatTest.java"
    )
    javac --release 11 -Xlint:all,-serial -Werror -d "${BUILD_DIR}/host-test/classes" "${PURE_SOURCES[@]}"
    java -cp "${BUILD_DIR}/host-test/classes" io.github.shinma06.arb.agent.ProtocolTest
    java -cp "${BUILD_DIR}/host-test/classes" io.github.shinma06.arb.agent.Aos9CompatTest
fi

# 2. Compile ARB Companion against android.jar (Java 8 bytecode -> d8 min-api 24)
echo "==> [2/3] Compiling ARB Companion Java sources -> DEX (min-api ${MIN_API})"
COMPANION_SOURCES=()
while IFS= read -r f; do
    COMPANION_SOURCES+=("${f}")
done < <(find "${SCRIPT_DIR}/companion/src" -name '*.java' | sort)

javac -source 8 -target 8 -Xlint:all,-options,-serial -Werror \
    -classpath "${ANDROID_JAR}" \
    -d "${BUILD_DIR}/companion/classes" \
    "${COMPANION_SOURCES[@]}"

CLASS_FILES=()
while IFS= read -r c; do
    CLASS_FILES+=("${c}")
done < <(find "${BUILD_DIR}/companion/classes" -name '*.class' | sort)

"${D8_BIN}" --release --min-api "${MIN_API}" \
    --lib "${ANDROID_JAR}" \
    --output "${BUILD_DIR}/companion/dex" \
    "${CLASS_FILES[@]}"

# Package deterministic ZIP/JAR (fixed mtime, sorted entries) using python3 zipfile
python3 - "${BUILD_DIR}/companion/dex/classes.dex" "${BUILD_DIR}/arb-companion.dex.jar" <<'PY'
import sys, zipfile
dex_path, jar_path = sys.argv[1], sys.argv[2]
with open(dex_path, "rb") as f:
    dex_bytes = f.read()
with zipfile.ZipFile(jar_path, "w", compression=zipfile.ZIP_DEFLATED) as zf:
    zi = zipfile.ZipInfo("classes.dex", date_time=(2020, 1, 1, 0, 0, 0))
    zi.compress_type = zipfile.ZIP_DEFLATED
    zi.external_attr = 0o644 << 16
    zf.writestr(zi, dex_bytes)
PY

COMPANION_SHA="$(shasum -a 256 "${BUILD_DIR}/arb-companion.dex.jar" | awk '{print $1}')"
COMPANION_BYTES="$(wc -c < "${BUILD_DIR}/arb-companion.dex.jar" | tr -d ' ')"
echo "==> [3/3] Built ${BUILD_DIR}/arb-companion.dex.jar (${COMPANION_BYTES} bytes, sha256=${COMPANION_SHA})"

# 3. Optional Apache-2.0 scrcpy-server-v5.0.1 + patched AudioRecordReader recipe
if [[ "${WITH_SCRCPY_AUDIO}" -eq 1 ]]; then
    SCRCPY_SERVER="${REPO_ROOT}/plugin/src/main/resources/replay/deps/scrcpy-server-v5.0.1"
    AUDIO_PATCH_SRC="${REPO_ROOT}/plugin/src/scrcpy/java/com/genymobile/scrcpy/audio/AudioRecordReader.java"
    EXPECTED_SCRCPY_SHA="764eb6f79811d5211fe9df341120882ba9994c7a61b897d7bf3fb662e53bc536"

    if [[ ! -f "${SCRCPY_SERVER}" || ! -f "${AUDIO_PATCH_SRC}" ]]; then
        echo "ERROR: scrcpy-server-v5.0.1 or AudioRecordReader.java not found in plugin/" >&2
        exit 1
    fi
    ACTUAL_SHA="$(shasum -a 256 "${SCRCPY_SERVER}" | awk '{print $1}')"
    if [[ "${ACTUAL_SHA}" != "${EXPECTED_SCRCPY_SHA}" ]]; then
        echo "ERROR: scrcpy-server-v5.0.1 SHA-256 mismatch: expected=${EXPECTED_SCRCPY_SHA} actual=${ACTUAL_SHA}" >&2
        exit 1
    fi

    echo "==> Building Apache-2.0 replay-scrcpy-audio.dex.jar (patched AudioRecordReader + verified scrcpy-server-v5.0.1)"
    mkdir -p "${BUILD_DIR}/scrcpy-audio/classes" "${BUILD_DIR}/scrcpy-audio/dex"
    javac -source 8 -target 8 -Xlint:all,-options -Werror \
        -classpath "${ANDROID_JAR}" \
        -d "${BUILD_DIR}/scrcpy-audio/classes" \
        "${AUDIO_PATCH_SRC}"
    "${D8_BIN}" --release --min-api "${MIN_API}" \
        --lib "${ANDROID_JAR}" \
        --output "${BUILD_DIR}/scrcpy-audio/dex" \
        "${BUILD_DIR}/scrcpy-audio/classes/com/genymobile/scrcpy/audio/AudioRecordReader.class"

    python3 - "${BUILD_DIR}/scrcpy-audio/dex/classes.dex" "${SCRCPY_SERVER}" "${BUILD_DIR}/replay-scrcpy-audio.dex.jar" <<'PY'
import sys, zipfile
patch_dex, server_jar, out_jar = sys.argv[1], sys.argv[2], sys.argv[3]
with open(patch_dex, "rb") as f:
    patch_bytes = f.read()
with zipfile.ZipFile(server_jar, "r") as src, zipfile.ZipFile(out_jar, "w", compression=zipfile.ZIP_DEFLATED) as dst:
    entries = []
    entries.append(("classes.dex", patch_bytes))
    for info in src.infolist():
        if info.filename == "META-INF/MANIFEST.MF":
            continue
        name = "classes2.dex" if info.filename == "classes.dex" else info.filename
        entries.append((name, src.read(info.filename)))
    entries.sort(key=lambda x: x[0])
    for name, data in entries:
        zi = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
        zi.compress_type = zipfile.ZIP_DEFLATED
        zi.external_attr = 0o644 << 16
        dst.writestr(zi, data)
PY
    SCRCPY_OUT_SHA="$(shasum -a 256 "${BUILD_DIR}/replay-scrcpy-audio.dex.jar" | awk '{print $1}')"
    SCRCPY_OUT_BYTES="$(wc -c < "${BUILD_DIR}/replay-scrcpy-audio.dex.jar" | tr -d ' ')"
    echo "==> Built ${BUILD_DIR}/replay-scrcpy-audio.dex.jar (${SCRCPY_OUT_BYTES} bytes, sha256=${SCRCPY_OUT_SHA})"
fi

#!/usr/bin/env bash
# Builds the Linux release binaries (arbd, arb-cli, arb-tray and the arb_gate_bundle example) inside an
# ubuntu:22.04 container, so they only need glibc <= 2.35 / GLIBCXX <= 3.4.30 (the Ubuntu 22.04 LTS
# baseline). GitHub's `ubuntu-latest` moves on (26.04 = glibc 2.43) and binaries built on it refuse to
# start on every LTS release still in use. The host runner keeps doing tests, Gradle and the SDK;
# only these release binaries come from the container.
#
# Outputs land where `cargo build --release` would put them (core/target/release/{arbd,arb-cli,arb-tray},
# core/target/release/examples/arb_gate_bundle), so packaging steps do not change. The ELF baseline is
# then checked with `verify_release_bundle.py --check-elf-compat`, which prints the maximum GLIBC /
# GLIBCXX each binary needs.
#
# Environment:
#   ARB_LINUX_BUILD_IMAGE     base image (default ubuntu:22.04)
#   ARB_LINUX_BUILD_CACHE     cargo home, rustup and target dir, kept between runs in a per-image and architecture
#                             subdirectory (default ${XDG_CACHE_HOME:-$HOME/.cache}/arb-linux-release-container)
#   ARB_RUST_TOOLCHAIN        rustup toolchain, must support edition 2024 (default stable)
#   ARB_LINUX_BUILD_PLATFORM  docker --platform (default linux/amd64, the shipped Linux architecture)
#   ARB_LINUX_BUILD_OUT       where the binaries are copied (default core/target/release)
set -euo pipefail

check_nasm_build() {
    local log_dir="$1" asm_log
    asm_log="$log_dir/run/stdout"
    if [[ ! -f "$asm_log" ]]; then asm_log="$log_dir/output"; fi
    if [[ ! -s "$asm_log" ]]; then
        echo "Missing openh264-sys2 build-script output in $log_dir" >&2
        return 1
    fi
    if grep -Ei "not using any assembly|NASM compilation disabled" "$asm_log"; then
        return 1
    else
        [[ $? = 1 ]]
    fi
}

if [[ "${1:-}" = --self-test ]]; then
    tmp="$(mktemp -d)"
    trap 'rm -rf -- "$tmp"' EXIT
    if check_nasm_build "$tmp"; then exit 1; fi
    mkdir -p "$tmp/run"
    for log_file in "$tmp/output" "$tmp/run/stdout"; do
        echo "cargo:rustc-link-lib=static=openh264_common" > "$log_file"
        check_nasm_build "$tmp"
        echo "Failed to compile NASM files, not using any assembly." >> "$log_file"
        if check_nasm_build "$tmp"; then exit 1; fi
        rm "$log_file"
    done
    echo "NASM build log self-test passed"
    exit 0
fi

if [[ "${1:-}" = --inside-container ]]; then
    export HOME=/cache/home
    export CARGO_HOME=/cache/cargo RUSTUP_HOME=/cache/rustup CARGO_TARGET_DIR=/cache/target
    export PATH="/cache/cargo/bin:${PATH}"
    if ! command -v rustup >/dev/null; then
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
    fi
    rustup toolchain install "${ARB_RUST_TOOLCHAIN}" --profile minimal --no-self-update
    rustup default "${ARB_RUST_TOOLCHAIN}"
    rustc --version
    ldd --version | sed -n 1p
    cargo build --locked --manifest-path core/Cargo.toml --release -p arbd -p arb-cli -p arb-tray \
        --message-format=json-render-diagnostics > /tmp/build.jsonl
    # Even on a cache hit Cargo identifies the active build-script output. openh264-sys2
    # silently falls back to C when assembly fails, so check its recorded stdout before shipping.
    asm_out="$(jq -ser '
        [.[] | select(.reason == "build-script-executed" and (.package_id | contains("#openh264-sys2@"))) | .out_dir]
        | unique | select(length == 1) | .[0]' /tmp/build.jsonl)"
    if ! check_nasm_build "${asm_out%/out}"; then
        cargo clean --manifest-path core/Cargo.toml --release -p openh264-sys2
        exit 1
    fi
    cargo build --locked --manifest-path core/Cargo.toml --release -p arbd --example arb_gate_bundle
    cp /cache/target/release/arbd /cache/target/release/arb-cli /cache/target/release/arb-tray /out/
    cp /cache/target/release/examples/arb_gate_bundle /out/examples/
    exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${CORE_DIR}/.." && pwd)"
IMAGE="${ARB_LINUX_BUILD_IMAGE:-ubuntu:22.04}"
CACHE_DIR="${ARB_LINUX_BUILD_CACHE:-${XDG_CACHE_HOME:-${HOME}/.cache}/arb-linux-release-container}"
TOOLCHAIN="${ARB_RUST_TOOLCHAIN:-stable}"
OUT_DIR="${ARB_LINUX_BUILD_OUT:-${CORE_DIR}/target/release}"

platform="${ARB_LINUX_BUILD_PLATFORM:-linux/amd64}"
docker pull --platform "${platform}" --quiet "${IMAGE}"
image_id="$(docker image inspect --format '{{.Id}}' "${IMAGE}")"
architecture="$(docker run --rm --platform "${platform}" "${image_id}" uname -m)"
[[ "${architecture}" = x86_64 ]] || { echo "expected x86_64, got ${architecture}" >&2; exit 1; }
CACHE_DIR="${CACHE_DIR}/${architecture}/${image_id#sha256:}"
mkdir -p "${CACHE_DIR}/cargo" "${CACHE_DIR}/rustup" "${CACHE_DIR}/target" "${CACHE_DIR}/home" "${OUT_DIR}/examples"

echo "==> building the Linux release binaries in ${IMAGE} (Rust ${TOOLCHAIN}, cache ${CACHE_DIR})"
docker run --rm --platform "${platform}" \
    -e ARB_RUST_TOOLCHAIN="${TOOLCHAIN}" \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -e CARGO_INCREMENTAL=0 -e CARGO_TERM_COLOR=never \
    -v "${REPO_ROOT}:/src:ro" -v "${CACHE_DIR}:/cache" -v "${OUT_DIR}:/out" \
    -w /src "${image_id}" bash -euo pipefail -c '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        # build-essential: gcc/g++/make for openh264 and the bundled SQLite; nasm gives openh264 its SIMD paths.
        apt-get install -y -qq --no-install-recommends build-essential nasm curl ca-certificates pkg-config jq >/dev/null
        nasm -v
        exec setpriv --reuid="${HOST_UID}" --regid="${HOST_GID}" --clear-groups bash core/scripts/build-linux-release-container.sh --inside-container
    '

echo "==> ELF baseline of the built binaries"
python3 "${SCRIPT_DIR}/verify_release_bundle.py" --check-elf-compat \
    "${OUT_DIR}/arbd" "${OUT_DIR}/arb-cli" "${OUT_DIR}/arb-tray" "${OUT_DIR}/examples/arb_gate_bundle"

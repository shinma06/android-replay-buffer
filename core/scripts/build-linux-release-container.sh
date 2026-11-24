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
#   ARB_LINUX_BUILD_CACHE     cargo home, rustup and target dir, kept between runs in a per-architecture
#                             subdirectory (default ${XDG_CACHE_HOME:-$HOME/.cache}/arb-linux-release-container)
#   ARB_RUST_TOOLCHAIN        rustup toolchain, must support edition 2024 (default stable)
#   ARB_LINUX_BUILD_PLATFORM  docker --platform, e.g. linux/amd64 on an Apple Silicon machine
#   ARB_LINUX_BUILD_OUT       where the binaries are copied (default core/target/release)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${CORE_DIR}/.." && pwd)"
IMAGE="${ARB_LINUX_BUILD_IMAGE:-ubuntu:22.04}"
CACHE_DIR="${ARB_LINUX_BUILD_CACHE:-${XDG_CACHE_HOME:-${HOME}/.cache}/arb-linux-release-container}"
TOOLCHAIN="${ARB_RUST_TOOLCHAIN:-stable}"
OUT_DIR="${ARB_LINUX_BUILD_OUT:-${CORE_DIR}/target/release}"

platform=()
if [[ -n "${ARB_LINUX_BUILD_PLATFORM:-}" ]]; then platform=(--platform "${ARB_LINUX_BUILD_PLATFORM}"); fi
# rustup, cargo and the target dir hold native executables: keep one cache per container architecture.
CACHE_DIR="${CACHE_DIR}/$(docker run --rm ${platform[@]+"${platform[@]}"} "${IMAGE}" uname -m)"
mkdir -p "${CACHE_DIR}/cargo" "${CACHE_DIR}/rustup" "${CACHE_DIR}/target" "${OUT_DIR}/examples"

echo "==> building the Linux release binaries in ${IMAGE} (Rust ${TOOLCHAIN}, cache ${CACHE_DIR})"
docker run --rm ${platform[@]+"${platform[@]}"} \
    -e ARB_RUST_TOOLCHAIN="${TOOLCHAIN}" \
    -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -e CARGO_INCREMENTAL=0 -e CARGO_TERM_COLOR=never \
    -v "${REPO_ROOT}:/src:ro" -v "${CACHE_DIR}:/cache" -v "${OUT_DIR}:/out" \
    -w /src "${IMAGE}" bash -euo pipefail -c '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        # build-essential: gcc/g++/make for openh264 and the bundled SQLite; nasm gives openh264 its SIMD paths.
        apt-get install -y -qq --no-install-recommends build-essential nasm curl ca-certificates pkg-config >/dev/null
        export CARGO_HOME=/cache/cargo RUSTUP_HOME=/cache/rustup CARGO_TARGET_DIR=/cache/target
        export PATH="/cache/cargo/bin:${PATH}"
        if ! command -v rustup >/dev/null; then
            curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
        fi
        rustup toolchain install "${ARB_RUST_TOOLCHAIN}" --profile minimal --no-self-update
        rustup default "${ARB_RUST_TOOLCHAIN}"
        rustc --version
        ldd --version | head -n 1
        cargo build --manifest-path core/Cargo.toml --release -p arbd -p arb-cli -p arb-tray
        cargo build --manifest-path core/Cargo.toml --release -p arbd --example arb_gate_bundle
        cp /cache/target/release/arbd /cache/target/release/arb-cli /cache/target/release/arb-tray /out/
        cp /cache/target/release/examples/arb_gate_bundle /out/examples/
        # The container runs as root: hand every file it created back to the host user.
        chown -R "${HOST_UID}:${HOST_GID}" /cache
        chown "${HOST_UID}:${HOST_GID}" /out/arbd /out/arb-cli /out/arb-tray /out/examples/arb_gate_bundle
    '

echo "==> ELF baseline of the built binaries"
python3 "${SCRIPT_DIR}/verify_release_bundle.py" --check-elf-compat \
    "${OUT_DIR}/arbd" "${OUT_DIR}/arb-cli" "${OUT_DIR}/arb-tray" "${OUT_DIR}/examples/arb_gate_bundle"

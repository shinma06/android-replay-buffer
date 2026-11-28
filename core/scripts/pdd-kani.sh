#!/usr/bin/env bash
set -euo pipefail

# Runs every mandatory `#[kani::proof]` harness of one crate (default `arb-core`); extra
# arguments go to `cargo kani` (for example `--harness k23_`). The `kani` matrix of
# `.github/workflows/pdd-kani.yml` calls this once per crate, so a new harness is registered by
# its crate's row: `arb-media` includes K23 (JPEG tile size, `src/jpeg.rs`, #586-G).
# Harnesses behind `cfg_attr(feature = "kani-extended", ...)` need `--features kani-extended`.

cd "$(dirname "$0")/.."
crate="${1:-arb-core}"
if (( $# )); then shift; fi
exec cargo kani -p "$crate" -Z stubbing "$@"

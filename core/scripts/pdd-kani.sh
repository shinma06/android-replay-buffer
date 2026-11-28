#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
crate="${1:-arb-core}"
if (( $# )); then shift; fi
exec cargo kani -p "$crate" -Z stubbing "$@"

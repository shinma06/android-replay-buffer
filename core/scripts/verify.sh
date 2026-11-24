#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${CORE_DIR}/.." && pwd)"

if command -v sara >/dev/null 2>&1; then
    SARA_BIN="$(command -v sara)"
elif [[ -x "${HOME}/.cargo/bin/sara" ]]; then
    SARA_BIN="${HOME}/.cargo/bin/sara"
else
    echo "ERROR: sara not found in PATH or ~/.cargo/bin/sara. Install with: cargo install sara-cli --no-default-features --locked" >&2
    exit 1
fi

echo "==> [1/6] cargo fmt --check"
cargo fmt --manifest-path "${CORE_DIR}/Cargo.toml" --all --check

echo "==> [2/6] cargo clippy --all-targets -- -D warnings"
cargo clippy --manifest-path "${CORE_DIR}/Cargo.toml" --workspace --all-targets -- -D warnings

echo "==> [3/6] cargo test"
cargo test --manifest-path "${CORE_DIR}/Cargo.toml" --workspace

echo "==> [4/6] license check (GPL/AGPL-free guard)"
python3 "${CORE_DIR}/scripts/check-licenses.py" --manifest-path "${CORE_DIR}/Cargo.toml"

echo "==> [5/6] sara check --strict"
(cd "${REPO_ROOT}" && "${SARA_BIN}" check --strict -c docs/sara.toml)

echo "==> [6/6] REQ -> downstream SPEC coverage check"
(cd "${REPO_ROOT}" && "${SARA_BIN}" report matrix --format json -c docs/sara.toml) | python3 -c '
import json, sys
data = json.load(sys.stdin)
reqs = {r["source_id"] for r in data.get("rows", []) if r.get("source_type") == "Requirement"}
satisfied = {
    t["id"]
    for r in data.get("rows", [])
    if r.get("source_type") == "Specification"
    for t in r.get("targets", [])
    if t.get("relationship") == "satisfies" and t.get("target_type") == "Requirement"
}
if not reqs:
    print("ERROR: no Requirement items found in sara traceability graph", file=sys.stderr)
    sys.exit(1)
orphans = sorted(reqs - satisfied)
if orphans:
    joined = ", ".join(orphans)
    print(f"ERROR: orphan REQ items without downstream SPEC: {joined}", file=sys.stderr)
    sys.exit(1)
print(f"req-coverage-check: OK ({len(reqs)}/{len(reqs)} REQ items satisfied by downstream SPEC)")
'

echo "==> ALL VERIFICATION GATES PASSED"

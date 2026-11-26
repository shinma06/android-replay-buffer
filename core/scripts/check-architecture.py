#!/usr/bin/env python3
"""Layer-4 PDD Rust structural guards (RS-1..RS-5).

Pure stdlib + `cargo metadata`. Scans non-test production Rust only: `#[cfg(test)]` /
`#[cfg(kani)]` items and `*tests.rs` files are stripped before matching.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

CORE = Path(__file__).resolve().parent.parent
REPO = CORE.parent
CRATES = CORE / "crates"

LIB_CRATES = ["arb-core", "arb-os", "arb-storage", "arb-media", "arb-adb"]
FORBID_UNSAFE = {"arb-core": ["lib.rs"], "arb-storage": ["lib.rs"], "arb-adb": ["lib.rs"],
                 "arb-cli": ["main.rs"], "arbd": ["lib.rs", "main.rs"]}
DENY_UNSAFE = {"arb-media": ["lib.rs"], "arb-tray": ["lib.rs", "main.rs"]}
UNSAFE_ALLOW_FILES = {"arb-media/src/codec/videotoolbox_backend.rs", "arb-tray/src/native_tray.rs"}
# Crates whose own code is the OS boundary (SPEC-01) and therefore may branch on the OS.
OS_CFG_ALLOWED_PREFIXES = ("arb-os/", "arb-media/src/codec/", "arb-tray/src/native_tray.rs")
RENAME_ALLOWED = {"arb-os/src/fs.rs"}
# RPCs intentionally not exposed to GUI/CLI/MCP clients (none today; add with a reason).
RPC_EXCLUDED: dict[str, str] = {}
# Strings in client sources that look like RPC names but are not.
CLIENT_NON_RPC = {"timeline.sqlite"}

OS_CFG = re.compile(r"\bcfg(?:_attr)?!?\(.*\b(?:target_os|target_family|unix|windows)\b")
CORE_IMPURE = re.compile(
    r"\bstd::(?:fs|net|process|thread)\b|\bSystemTime\b|\bInstant\b|"
    r"\bcfg(?:_attr)?!?\(.*\b(?:target_os|target_family|unix|windows)\b"
)
RPC_NAME = re.compile(r'"((?:system|config|device|capture|save|bundle|timeline|edit|export|media)\.[a-z_]+)"')

failures: list[str] = []


def fail(rule: str, msg: str) -> None:
    failures.append(f"{rule}: {msg}")


def strip_comments(src: str) -> str:
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
    return "\n".join(re.sub(r"//.*$", "", line) for line in src.split("\n"))


def strip_test_items(src: str) -> str:
    """Blank out items gated by `#[cfg(test)]` / `#[cfg(kani)]` (keeps line numbers)."""
    out = list(src)
    for m in re.finditer(r"#\[cfg\((?:test|kani|all\(test[^\]]*)\)\]", src):
        i = src.find("{", m.end())
        semi = src.find(";", m.end())
        if i < 0 or (0 <= semi < i):
            continue
        depth, j = 0, i
        while j < len(src):
            if src[j] == "{":
                depth += 1
            elif src[j] == "}":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        for k in range(m.start(), min(j + 1, len(src))):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def join_cfg_predicates(src: str) -> str:
    """Folds each multi-line `cfg(...)` / `cfg_attr(...)` / `cfg!(...)` onto its first line.

    The removed newlines are re-inserted right after the closing parenthesis, so every later
    line keeps its number while line-based rules still see the whole OS predicate.
    """
    out: list[str] = []
    pos = 0
    for m in re.finditer(r"\bcfg(?:_attr)?!?\(", src):
        if m.start() < pos:
            continue
        depth, j = 0, m.end() - 1
        while j < len(src):
            if src[j] == "(":
                depth += 1
            elif src[j] == ")":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        pred = src[m.start():j + 1]
        out.append(src[pos:m.start()])
        out.append(pred.replace("\n", " ") + "\n" * pred.count("\n"))
        pos = j + 1
    out.append(src[pos:])
    return "".join(out)


def production_sources():
    for path in sorted(CRATES.glob("*/src/**/*.rs")):
        rel = path.relative_to(CRATES).as_posix()
        if path.name.endswith("tests.rs") or "/tests/" in rel or path.name == "kani_proofs.rs":
            continue
        src = join_cfg_predicates(strip_test_items(strip_comments(path.read_text(encoding="utf-8"))))
        yield rel, src


# Raw strings, strings and char literals, matched left to right so a quote inside one
# literal never pairs with code after it (e.g. `feature = "windows"`, `'"'`, `r#"'"'"#`).
LITERAL = re.compile(r'r(#*)".*?"\1|"(?:[^"\\]|\\.)*"' + r"|'(?:[^'\\]|\\.)'")


def grep(src: str, pattern: re.Pattern[str]):
    for n, line in enumerate(src.split("\n"), 1):
        if pattern.search(LITERAL.sub("_", line)):
            yield n, line.strip()


def rs1_dag() -> None:
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps",
         "--manifest-path", str(CORE / "Cargo.toml")],
        check=True, capture_output=True, text=True, encoding="utf-8").stdout)
    deps = {p["name"]: {d["name"] for d in p["dependencies"] if d["kind"] is None}
            for p in meta["packages"]}
    extra = deps.get("arb-core", set()) - {"serde", "serde_json", "thiserror"}
    if extra:
        fail("RS-1", f"arb-core has non-pure normal deps {sorted(extra)}")
    for front in ("arb-cli", "arb-tray"):
        bad = deps.get(front, set()) & {"arbd", "arb-storage", "arb-media", "arb-adb"}
        if bad:
            fail("RS-1", f"{front} depends on engine crates {sorted(bad)} (must go through arbd IPC)")
    lateral = {"arb-storage", "arb-media", "arb-adb"}
    for c in lateral:
        bad = deps.get(c, set()) & (lateral - {c})
        if bad:
            fail("RS-1", f"lateral dependency {c} -> {sorted(bad)}")


def main() -> int:
    rs1_dag()
    rpc_dispatched: set[str] = set()
    for rel, src in production_sources():
        crate = rel.split("/", 1)[0]
        if crate == "arb-core":
            for n, line in grep(src, CORE_IMPURE):
                fail("RS-1", f"{rel}:{n}: arb-core must stay pure: {line}")
        elif not rel.startswith(OS_CFG_ALLOWED_PREFIXES):
            for n, line in grep(src, OS_CFG):
                fail("RS-2", f"{rel}:{n}: OS cfg outside arb-os / codec / native tray: {line}")
        if rel not in RENAME_ALLOWED:
            for n, line in grep(src, re.compile(r"\bfs::rename\s*\(")):
                fail("RS-4", f"{rel}:{n}: use arb_os::fs::{{replace_file,rename_no_replace}}: {line}")
        if rel not in UNSAFE_ALLOW_FILES and crate in DENY_UNSAFE:
            for n, line in grep(src, re.compile(r"allow\(\s*unsafe_code")):
                fail("RS-3", f"{rel}:{n}: unsafe_code allowed outside the FFI boundary: {line}")
        if rel == "arbd/src/lib.rs":
            m = re.search(r"fn dispatch\(\s*(?:&self|self: &Arc<Self>),\s*method: &str.*?\n    }\n", src, re.S)
            if m:
                rpc_dispatched = set(RPC_NAME.findall(m.group(0)))

    for crate, roots in FORBID_UNSAFE.items():
        for root in roots:
            if "#![forbid(unsafe_code)]" not in (CRATES / crate / "src" / root).read_text(encoding="utf-8"):
                fail("RS-3", f"{crate}/src/{root} lacks #![forbid(unsafe_code)]")
    for crate, roots in DENY_UNSAFE.items():
        for root in roots:
            if "#![deny(unsafe_code)]" not in (CRATES / crate / "src" / root).read_text(encoding="utf-8"):
                fail("RS-3", f"{crate}/src/{root} lacks #![deny(unsafe_code)]")

    guard = "#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]"
    for crate in LIB_CRATES:
        if guard not in (CRATES / crate / "src" / "lib.rs").read_text(encoding="utf-8"):
            fail("RS-5", f"{crate}/src/lib.rs lacks `{guard}`")

    if not rpc_dispatched:
        fail("RS-5", "could not locate arbd dispatch table in arbd/src/lib.rs")
    clients: set[str] = set()
    for kt in (REPO / "ide-plugin").glob("**/src/main/kotlin/**/*.kt"):
        clients |= set(RPC_NAME.findall(kt.read_text(encoding="utf-8")))
    for rs in (CRATES / "arb-cli" / "src").glob("*.rs"):
        clients |= set(RPC_NAME.findall(rs.read_text(encoding="utf-8")))
    # The catalog reconciliation and purge operations are required on each surface.
    for source in (
        next((REPO / "ide-plugin").rglob("src/main/kotlin/**/ArbdRpcClient.kt")),
        CRATES / "arb-cli/src/main.rs",
        CRATES / "arb-cli/src/mcp.rs",
    ):
        for method in sorted({"bundle.gc", "bundle.forget"} - set(RPC_NAME.findall(source.read_text(encoding="utf-8")))):
            fail("RS-5", f"{source.name} lacks catalog RPC `{method}`")
    clients -= CLIENT_NON_RPC
    for method in sorted(rpc_dispatched - clients - set(RPC_EXCLUDED)):
        fail("RS-5", f"arbd RPC `{method}` has no GUI (ArbdRpcClient) / CLI / MCP caller")
    for method in sorted(clients - rpc_dispatched):
        fail("RS-5", f"client calls `{method}` which arbd does not dispatch")

    if failures:
        for f in failures:
            print(f"check-architecture: {f}", file=sys.stderr)
        print(f"check-architecture: FAILED ({len(failures)} violation(s))", file=sys.stderr)
        return 1
    print(f"check-architecture: OK (RS-1..RS-5; {len(rpc_dispatched)} RPC methods in parity)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

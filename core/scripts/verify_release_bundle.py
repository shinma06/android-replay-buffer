#!/usr/bin/env python3
"""Cold-install verification of the *shipped* Android Replay Buffer (ARB) release ZIPs (G-M3 / G-M4).

Pure Python 3 stdlib (Linux / macOS / Windows, UTF-8 safe). The harness never uses the repository's
build output: it extracts `arb-<os>-<arch>.zip` (and optionally the universal `arb-ide-plugin-*.zip`)
into an isolated temporary directory outside the repository and drives only the extracted
`bin/arbd`, `bin/arb-cli` and `bin/arb-tray` with `ARB_DATA_DIR` / `ARB_CACHE_DIR` /
`ARB_RUNTIME_DIR` pointing into that directory.

Sections (each one is reported separately, a crash in one never hides the others)
  1  bundle-integrity   SHA256SUMS.txt (every file, both ways), POSIX exec bits, LICENSE-MIT /
                        LICENSE-APACHE / THIRD_PARTY_NOTICES.md, retired product-version label,
                        universal plugin ZIP (3 OS native dirs, device-agent DEX jars, licenses,
                        plugin.xml metadata, byte-identical host binaries)
  2  launchers          start-arb.sh / start-arb-tray.command / start-arb.ps1
  3  cold-start-ipc     arbd cold start, `status`, `config get` / `config set`, `bundles list`
  4  mcp                `arb-cli mcp`: initialize, 19 tools, stderr 0 B, real-bundle overview + contact sheet
  5  legacy-read-only   schema 3 / schema 4 bundles: every file SHA-256 identical before and after
  6  suite-b            devin_verify_player_matrix.py 17/17 for 1 s GOP and 5 s GOP FHD30 `[47..=647]`
                        mid-GOP trims + open / seek / trim timings
  7  burn-in            share.mp4 vs share-taps.mp4 with Tap + Swipe: decoded RGB pixel proof
  8  tray (--check-tray) arb-tray cold start, single instance, clean exit
  9  upgrade            G-M9 U3/U4: a running arbd of an older build (a real previous release with
                        --upgrade-from-zip, otherwise this very build with pseudo-old state) is replaced
                        in place by the new launcher; config.json (missing + unknown fields,
                        capture_enabled), catalog.sqlite (saved / trimmed / trashed bundles, a
                        retry-pending job) and a pre-T-6H 5-column timeline.sqlite survive; with
                        --check-tray a running arb-tray survives the arbd restart (U4). A previous release whose
                        arbd is byte-identical to the candidate fails, unless --allow-same-previous-arbd (a
                        reproducible build with core/ unchanged): then the upgrade runs against a copy of it
                        with a distinct build_id, so U1-U4 still see a real replacement.

Fixture bundles come from `cargo run --release -p arbd --example arb_gate_bundle` (a deterministic
synthetic FHD30 bundle with audio, Tap and Swipe gestures); see `--fixtures-dir` / `--generator`.

Outputs (`--out-dir`): release-bundle-report.json, release-bundle-report.md, per-suite devin reports.
Exit code 0 only when every check passes.
"""

from __future__ import annotations

import argparse
import base64
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import platform
import queue
import re
import shutil
import sqlite3
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
from pathlib import Path
from typing import Any, Callable, Iterator, Sequence

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent

BINARIES = ("arbd", "arb-cli", "arb-tray")
DEVICE_AGENT_JARS = ("arb-companion.dex.jar", "replay-scrcpy-audio.dex.jar")
LICENSE_FILES = ("LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_NOTICES.md")
PLUGIN_LICENSE_DIR = "licenses"
UNIVERSAL_NATIVE_SLUGS = ("linux-x86_64", "macos-arm64", "windows-x86_64")
BUNDLE_RE = re.compile(r"^arb-(linux|macos|windows)-([a-z0-9_]+)$")
EXPECTED_MCP_TOOLS = (
    "arb_status",
    "arb_devices",
    "arb_capture_start",
    "arb_capture_stop",
    "arb_config_get",
    "arb_config_update",
    "arb_save",
    "arb_screenshot",
    "arb_save_logs",
    "arb_list_bundles",
    "arb_trash_bundle",
    "arb_restore_bundle",
    "arb_gc_bundles",
    "arb_forget_bundle",
    "arb_timeline_overview",
    "arb_query_logs",
    "arb_inspect_frames",
    "arb_export_trim",
    "arb_export_frame_png",
)
# Same patterns as ide-plugin's ArbBrandingGuardTest: the retired product-version label.
RETIRED_LABEL_PATTERNS = (
    re.compile(r"ARB\s+v2", re.IGNORECASE),
    re.compile(r"\(v2\)", re.IGNORECASE),
    re.compile(r"Android Replay Buffer\s+v2", re.IGNORECASE),
    re.compile(r"v2\s*バンドル", re.IGNORECASE),
    re.compile(r"v2\s+WORM", re.IGNORECASE),
    re.compile(r"arb-v2-", re.IGNORECASE),
)
EXPECTED_MATRIX_CHECKS = 17
TRIM_FRAMES = 601
PIXEL_THRESHOLD = 30  # per-channel absolute difference that counts as a changed pixel
MARKER_MIN_PIXELS = 300  # of a (2*half)^2 window: a real marker changes >1,000
CONTROL_MAX_PIXELS = 5


# --------------------------------------------------------------------------------------------
# platform helpers
# --------------------------------------------------------------------------------------------

def normalize_os(raw: str | None = None) -> str:
    value = (raw or platform.system()).strip().lower()
    if value in ("darwin", "macos", "osx"):
        return "macos"
    if value == "linux":
        return "linux"
    if value in ("windows", "win32", "win"):
        return "windows"
    raise ValueError(f"unsupported OS {raw!r}")


def normalize_arch(raw: str | None = None) -> str:
    value = (raw or platform.machine()).strip().lower()
    if value in ("arm64", "aarch64"):
        return "arm64"
    if value in ("x86_64", "amd64", "x64"):
        return "x86_64"
    return value or "unknown"


def exe(name: str, target_os: str) -> str:
    return f"{name}.exe" if target_os == "windows" else name


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


# Linux binaries must start on the oldest glibc baseline we ship for (Ubuntu 22.04 LTS: glibc 2.35,
# libstdc++ GLIBCXX_3.4.30). GitHub's `ubuntu-latest` moves on (26.04 = glibc 2.43), so a bundle built
# there silently stops launching on every LTS release still in use.
MAX_GLIBC = (2, 35)
MAX_GLIBCXX = (3, 4, 30)
SHT_GNU_VERNEED = 0x6FFFFFFE


def elf_max_symbol_versions(data: bytes) -> dict[str, tuple[int, ...]]:
    """Highest versioned dependency per library family (`GLIBC`, `GLIBCXX`, `GCC`, ...) an ELF64
    little-endian binary requires: the `.gnu.version_r` table that `readelf -V` prints.
    Raises ValueError for anything that is not such an ELF file."""
    if data[:4] != b"\x7fELF" or len(data) < 64 or data[4] != 2 or data[5] != 1:
        raise ValueError("not an ELF64 little-endian file")
    shoff, = struct.unpack_from("<Q", data, 0x28)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x3A)
    if shnum == 0 or shentsize < 64 or shoff == 0 or shoff + shentsize * shnum > len(data):
        raise ValueError("truncated ELF section header table")

    def section(index: int) -> tuple[int, int, int, int, int]:
        _, sh_type, _, _, offset, size, link, info, *_ = struct.unpack_from("<IIQQQQIIQQ", data, shoff + index * shentsize)
        return sh_type, offset, size, link, info

    found: dict[str, tuple[int, ...]] = {}
    for index in range(shnum):
        sh_type, offset, size, link, count = section(index)
        if sh_type != SHT_GNU_VERNEED:
            continue
        if link >= shnum or offset + size > len(data):
            raise ValueError("invalid version requirements section")
        table_type, strtab, strsize, _, _ = section(link)
        if table_type != 3 or strtab + strsize > len(data):
            raise ValueError("invalid version requirements string table")
        entry = offset
        for i in range(count):
            if entry + 16 > offset + size:
                raise ValueError("truncated version requirement")
            _, aux_count, _, aux_offset, next_entry = struct.unpack_from("<HHIII", data, entry)
            if i + 1 < count and next_entry < 16:
                raise ValueError("invalid version requirement chain")
            # Linkers may group all Verneed headers first and place all Vernaux
            # lists afterwards. vn_next bounds the header chain, not the aux list.
            if entry + next_entry > offset + size or (aux_count and aux_offset < 16):
                raise ValueError("invalid version requirement offsets")
            aux = entry + aux_offset
            for j in range(aux_count):
                if aux + 16 > offset + size:
                    raise ValueError("truncated auxiliary version requirement")
                _, _, _, name_offset, next_aux = struct.unpack_from("<IHHII", data, aux)
                if j + 1 < aux_count and next_aux < 16:
                    raise ValueError("invalid auxiliary version requirement chain")
                start = strtab + name_offset
                name = data[start:data.index(b"\0", start, strtab + strsize)].decode("ascii")
                family, _, number = name.rpartition("_")
                if family and re.fullmatch(r"\d+(\.\d+)*", number):
                    version = tuple(int(part) for part in number.split("."))
                    found[family] = max(found.get(family, ()), version)
                aux += next_aux
            entry += next_entry
    return found


def elf_compat_problems(data: bytes) -> tuple[str, list[str]]:
    """(summary, problems) for the glibc / libstdc++ baseline of one Linux binary."""
    needs = elf_max_symbol_versions(data)

    def dotted(version: tuple[int, ...] | None) -> str:
        return ".".join(map(str, version)) if version else "none"

    summary = f"max GLIBC {dotted(needs.get('GLIBC'))} (limit {dotted(MAX_GLIBC)}), GLIBCXX {dotted(needs.get('GLIBCXX'))} (limit {dotted(MAX_GLIBCXX)})"
    problems = []
    if needs.get("GLIBC", ()) > MAX_GLIBC:
        problems.append(f"GLIBC_{dotted(needs['GLIBC'])} > GLIBC_{dotted(MAX_GLIBC)}")
    if needs.get("GLIBCXX", ()) > MAX_GLIBCXX:
        problems.append(f"GLIBCXX_{dotted(needs['GLIBCXX'])} > GLIBCXX_{dotted(MAX_GLIBCXX)}")
    return summary, problems


def snapshot_tree(root: Path) -> dict[str, str]:
    """Relative path -> SHA-256 for every file, plus `<dir>/` markers for every directory."""
    snap: dict[str, str] = {}
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root).as_posix()
        if path.is_dir():
            snap[rel + "/"] = "dir"
        elif path.is_file():
            snap[rel] = sha256_file(path)
        else:
            snap[rel] = "other"
    return snap


def diff_snapshots(before: dict[str, str], after: dict[str, str]) -> list[str]:
    problems = []
    for rel in sorted(set(before) | set(after)):
        if rel not in after:
            problems.append(f"removed: {rel}")
        elif rel not in before:
            problems.append(f"added: {rel}")
        elif before[rel] != after[rel]:
            problems.append(f"modified: {rel}")
    return problems


def png_dimensions(data: bytes) -> tuple[int, int] | None:
    if len(data) < 24 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        return None
    return struct.unpack(">II", data[16:24])


def retired_label_hits(text: str) -> list[str]:
    return [p.pattern for p in RETIRED_LABEL_PATTERNS if p.search(text)]


# --------------------------------------------------------------------------------------------
# report model
# --------------------------------------------------------------------------------------------

class Section:
    def __init__(self, name: str, title: str) -> None:
        self.name = name
        self.title = title
        self.checks: list[dict[str, Any]] = []
        self.seconds = 0.0
        self.notes: dict[str, Any] = {}

    def check(self, name: str, ok: bool, detail: Any = "") -> bool:
        self.checks.append({"name": name, "status": "PASS" if ok else "FAIL", "pass": bool(ok), "detail": str(detail)})
        if self.name == "upgrade":
            print(f"    [{'PASS' if ok else 'FAIL'}] {name}", flush=True)
        return bool(ok)

    def skip(self, name: str, reason: str) -> None:
        """Only for a truly OS-only facility that cannot exist on this host; always reported."""
        self.checks.append({"name": name, "status": "SKIP", "pass": True, "detail": reason})

    @property
    def passed(self) -> bool:
        return all(c["pass"] for c in self.checks) and bool(self.checks)


class Report:
    def __init__(self, host: dict[str, Any], quiet: bool = False) -> None:
        self.host = host
        self.quiet = quiet
        self.sections: list[Section] = []
        self.perf: dict[str, Any] = {}
        self.artifacts: list[str] = []

    @contextlib.contextmanager
    def section(self, name: str, title: str) -> Iterator[Section]:
        sec = Section(name, title)
        started = time.perf_counter()
        if not self.quiet:
            print(f"==> [{name}] {title}", flush=True)
        try:
            yield sec
        except Exception as exc:  # noqa: BLE001 - a crashed section must be reported, not abort the run
            import traceback

            sec.check(f"{name} completed without an exception", False, f"{type(exc).__name__}: {exc}\n{traceback.format_exc()[-1200:]}")
        finally:
            sec.seconds = round(time.perf_counter() - started, 2)
            self.sections.append(sec)
            bad = [c for c in sec.checks if not c["pass"]]
            if not self.quiet:
                print(f"    {'PASS' if sec.passed else 'FAIL'} ({len(sec.checks)} checks, {len(bad)} failed, {sec.seconds}s)", flush=True)
                for c in bad:
                    print(f"      FAIL {c['name']}: {c['detail'][:300]}", flush=True)

    @property
    def passed(self) -> bool:
        return bool(self.sections) and all(s.passed for s in self.sections)

    def to_json(self) -> dict[str, Any]:
        return {
            "host": self.host,
            "pass": self.passed,
            "perf": self.perf,
            "artifacts": self.artifacts,
            "sections": [
                {"name": s.name, "title": s.title, "pass": s.passed, "seconds": s.seconds, "notes": s.notes, "checks": s.checks}
                for s in self.sections
            ],
        }

    def to_markdown(self) -> str:
        lines = [
            "# リリース配布物の検証レポート (verify_release_bundle.py)",
            "",
            f"- 判定: **{'PASS' if self.passed else 'FAIL'}**",
            f"- ホスト: `{self.host.get('platform')}` / bundle `{self.host.get('bundle')}`",
            f"- コア版: `{self.host.get('core_version')}`",
            "",
            "| セクション | 結果 | チェック数 | 失敗 | 秒 |",
            "| --- | --- | ---: | ---: | ---: |",
        ]
        for s in self.sections:
            failed = sum(1 for c in s.checks if not c["pass"])
            lines.append(f"| {s.name} | {'PASS' if s.passed else 'FAIL'} | {len(s.checks)} | {failed} | {s.seconds} |")
        if self.perf:
            lines += ["", "## 性能 (Suite B)", "", "| GOP | open ms | filmstrip ms | scrub ms | seek ms | trim ms | trim+burn ms |", "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
            for gop, row in sorted(self.perf.items()):
                lines.append(
                    f"| {gop} | {row.get('open_ms', '')} | {row.get('filmstrip_ms', '')} | {row.get('scrub_ms', '')} | "
                    f"{row.get('seek_ms', '')} | {row.get('trim_ms', '')} | {row.get('trim_burn_ms', '')} |"
                )
        for s in self.sections:
            lines += ["", f"## {s.name}: {s.title}", "", "| チェック | 結果 | 詳細 |", "| --- | --- | --- |"]
            for c in s.checks:
                detail = c["detail"].replace("\n", " ").replace("|", "\\|")[:400]
                lines.append(f"| {c['name']} | {c['status']} | {detail} |")
        return "\n".join(lines) + "\n"


# --------------------------------------------------------------------------------------------
# process helpers
# --------------------------------------------------------------------------------------------

class CliResult:
    def __init__(self, rc: int, out: str, err: str, secs: float) -> None:
        self.rc, self.out, self.err, self.secs = rc, out, err, secs

    @property
    def data(self) -> Any:
        try:
            return json.loads(self.out)
        except ValueError:
            return None


def run_proc(cmd: Sequence[Any], env: dict[str, str] | None = None, timeout: float = 600, cwd: Path | None = None) -> CliResult:
    started = time.perf_counter()
    # A background daemon can inherit a launcher's stdio. A file lets us wait for
    # the foreground process alone, even when a descendant keeps the handle open.
    with (tempfile.TemporaryFile("w+", encoding="utf-8", errors="replace") as out,
          tempfile.TemporaryFile("w+", encoding="utf-8", errors="replace") as err):
        diagnostic = ""
        try:
            rc = subprocess.run(
                [str(c) for c in cmd], stdout=out, stderr=err, timeout=timeout, env=env, cwd=cwd,
            ).returncode
        except subprocess.TimeoutExpired:
            rc, diagnostic = 124, f"timeout after {timeout}s: "
        except OSError as exc:
            rc, diagnostic = 127, f"{type(exc).__name__}: {exc}"
        out.seek(0)
        err.seek(0)
        return CliResult(rc, out.read(), diagnostic + err.read(), time.perf_counter() - started)


def list_processes_under(root: Path) -> list[str]:
    """Lines describing live processes whose executable lives under `root` (excluding this process)."""
    me = os.getpid()
    if os.name == "nt":
        pattern = str(root).replace("'", "''")
        script = (
            f"Get-CimInstance Win32_Process | Where-Object {{ $_.ExecutablePath -like '{pattern}\\*' }} "
            "| ForEach-Object { \"$($_.ProcessId) $($_.ExecutablePath)\" }"
        )
        res = run_proc(["powershell", "-NoProfile", "-NonInteractive", "-Command", script], timeout=60)
        return [ln.strip() for ln in res.out.splitlines() if ln.strip() and not ln.strip().startswith(f"{me} ")]
    res = run_proc(["ps", "-axo", "pid=,args="], timeout=30)
    needle = str(root)
    hits = []
    for line in res.out.splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) == 2 and parts[0] != str(me) and needle in parts[1]:
            hits.append(line.strip())
    return hits


class McpClient:
    """Minimal newline-delimited JSON-RPC 2.0 client for `arb-cli mcp`."""

    def __init__(self, cmd: Sequence[Any], env: dict[str, str]) -> None:
        self.proc = subprocess.Popen(
            [str(c) for c in cmd],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        self._lines: queue.Queue[bytes | None] = queue.Queue()
        self._stderr = b""
        threading.Thread(target=self._pump_stdout, daemon=True).start()
        self._stderr_thread = threading.Thread(target=self._pump_stderr, daemon=True)
        self._stderr_thread.start()
        self._next_id = 1
        self.stray_frames: list[str] = []

    def _pump_stdout(self) -> None:
        assert self.proc.stdout is not None
        for line in iter(self.proc.stdout.readline, b""):
            self._lines.put(line)
        self._lines.put(None)

    def _pump_stderr(self) -> None:
        assert self.proc.stderr is not None
        self._stderr = self.proc.stderr.read()

    def request(self, method: str, params: dict[str, Any] | None = None, timeout: float = 120) -> dict[str, Any]:
        rid = self._next_id
        self._next_id += 1
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"MCP {method} timed out after {timeout}s")
            line = self._lines.get(timeout=remaining)
            if line is None:
                raise RuntimeError("MCP server closed stdout")
            frame = json.loads(line.decode("utf-8"))
            if frame.get("id") == rid:
                return frame
            self.stray_frames.append(line.decode("utf-8", "replace").strip())

    def notify(self, method: str) -> None:
        self._send({"jsonrpc": "2.0", "method": method})

    def _send(self, message: dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def call_tool(self, name: str, arguments: dict[str, Any], timeout: float = 180) -> dict[str, Any]:
        return self.request("tools/call", {"name": name, "arguments": arguments}, timeout)

    def close(self) -> bytes:
        with contextlib.suppress(Exception):
            assert self.proc.stdin is not None
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self._stderr_thread.join(timeout=10)
        return self._stderr


def tool_text(response: dict[str, Any]) -> Any:
    block = response["result"]["content"][0]
    return json.loads(block["text"])


# --------------------------------------------------------------------------------------------
# sibling script loading
# --------------------------------------------------------------------------------------------

def load_sibling(name: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, SCRIPT_DIR / f"{name}.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# --------------------------------------------------------------------------------------------
# section 1: bundle integrity
# --------------------------------------------------------------------------------------------

def safe_extract(zip_path: Path, dest: Path) -> list[zipfile.ZipInfo]:
    """Extracts `zip_path` into `dest`, refusing path traversal and restoring POSIX modes."""
    dest = dest.resolve()
    infos = []
    with zipfile.ZipFile(zip_path) as zf:
        for info in zf.infolist():
            target = (dest / info.filename).resolve()
            if dest != target and dest not in target.parents:
                raise ValueError(f"unsafe path in {zip_path.name}: {info.filename}")
            zf.extract(info, dest)
            mode = (info.external_attr >> 16) & 0o7777
            if os.name != "nt" and mode and not info.is_dir():
                os.chmod(target, mode)
            infos.append(info)
    return infos


def verify_sha256sums(zf: zipfile.ZipFile, root: str, sec: Section) -> None:
    sums_name = f"{root}/SHA256SUMS.txt"
    names = set(zf.namelist())
    if not sec.check("SHA256SUMS.txt present", sums_name in names, sums_name):
        return
    listed: dict[str, str] = {}
    for line in zf.read(sums_name).decode("utf-8").splitlines():
        digest, _, rel = line.partition("  ")
        listed[rel] = digest
    mismatches = [rel for rel, digest in listed.items() if rel not in names or sha256_bytes(zf.read(rel)) != digest]
    sec.check(f"SHA256SUMS.txt matches all {len(listed)} listed files", not mismatches, mismatches[:5])
    files = {n for n in names if not n.endswith("/") and n != sums_name}
    sec.check("every file in the ZIP is listed in SHA256SUMS.txt", files == set(listed), sorted(files ^ set(listed))[:5])


def verify_plugin_zip(
    plugin_zip: Path,
    sec: Section,
    label: str,
    *,
    universal: bool,
    host_slug: str,
    bundle_license_bytes: dict[str, bytes],
    bundle_bin: dict[str, bytes] | None,
) -> None:
    with zipfile.ZipFile(plugin_zip) as zf:
        names = zf.namelist()
        roots = {n.split("/", 1)[0] for n in names if n.strip("/")}
        if not sec.check(f"{label}: single plugin root folder", len(roots) == 1, sorted(roots)):
            return
        root = next(iter(roots))
        nameset = set(names)
        jars = [n for n in names if n.startswith(f"{root}/lib/") and n.endswith(".jar")]
        sec.check(f"{label}: lib/*.jar present", len(jars) >= 2, jars)
        for jar in DEVICE_AGENT_JARS:
            sec.check(f"{label}: device-agent/{jar}", f"{root}/device-agent/{jar}" in nameset)
        for lic in LICENSE_FILES:
            rel = f"{root}/{PLUGIN_LICENSE_DIR}/{lic}"
            ok = rel in nameset and (lic not in bundle_license_bytes or zf.read(rel) == bundle_license_bytes[lic])
            sec.check(f"{label}: {PLUGIN_LICENSE_DIR}/{lic} (identical to the bundle copy)", ok)
        slugs = UNIVERSAL_NATIVE_SLUGS if universal else (host_slug,)
        for slug in slugs:
            target_os = slug.split("-", 1)[0]
            for binary in BINARIES:
                rel = f"{root}/native/{slug}/{exe(binary, target_os)}"
                present = rel in nameset
                modes_ok = True
                if present and target_os != "windows":
                    modes_ok = ((zf.getinfo(rel).external_attr >> 16) & 0o111) == 0o111
                sec.check(f"{label}: native/{slug}/{exe(binary, target_os)}", present and modes_ok, "missing" if not present else ("not executable" if not modes_ok else ""))
        if universal:
            native_dirs = {n.split("/")[2] for n in names if n.startswith(f"{root}/native/") and n.count("/") >= 3}
            sec.check(f"{label}: exactly the 3 OS native dirs", native_dirs == set(UNIVERSAL_NATIVE_SLUGS), sorted(native_dirs))
        if bundle_bin:
            host_os = host_slug.split("-", 1)[0]
            same = all(
                f"{root}/native/{host_slug}/{exe(b, host_os)}" in nameset
                and zf.read(f"{root}/native/{host_slug}/{exe(b, host_os)}") == bundle_bin[b]
                for b in BINARIES
            )
            sec.check(f"{label}: native/{host_slug} binaries byte-identical to bundle bin/", same)
        main_jars = [j for j in jars if "arb-ide-plugin" in j.rsplit("/", 1)[-1] and "searchableOptions" not in j]
        if sec.check(f"{label}: main plugin jar found", bool(main_jars), jars) and main_jars:
            with zipfile.ZipFile(io.BytesIO(zf.read(main_jars[0]))) as jar:
                jar_names = set(jar.namelist())
                plugin_xml = jar.read("META-INF/plugin.xml").decode("utf-8") if "META-INF/plugin.xml" in jar_names else ""
                sec.check(f"{label}: plugin.xml <change-notes>", "<change-notes>" in plugin_xml)
                sec.check(f"{label}: plugin.xml description starts with Latin text", bool(re.search(r"<description>\s*(<!\[CDATA\[)?\s*[A-Za-z]", plugin_xml)))
                sec.check(f"{label}: optional dependencies declare config-file", 'config-file="arb-android.xml"' in plugin_xml and 'config-file="arb-java.xml"' in plugin_xml)
                sec.check(f"{label}: arb-android.xml / arb-java.xml packaged", {"META-INF/arb-android.xml", "META-INF/arb-java.xml"} <= jar_names)
                sec.check(f"{label}: idea-version since 262.9437.185 / until 262.*", 'since-build="262.9437.185"' in plugin_xml and 'until-build="262.*"' in plugin_xml)
                sec.check(f"{label}: plugin.xml has no retired version label", not retired_label_hits(plugin_xml), retired_label_hits(plugin_xml))


def section_bundle_integrity(report: Report, ctx: dict[str, Any]) -> None:
    with report.section("bundle-integrity", "配布 ZIP のクリーン展開と整合") as sec:
        zip_path: Path = ctx["bundle_zip"]
        match = re.match(r"^(arb-(?:linux|macos|windows)-[a-z0-9_]+)\.zip$", zip_path.name)
        if not sec.check("bundle ZIP is named arb-<os>-<arch>.zip", bool(match), zip_path.name):
            return
        stem = match.group(1)  # type: ignore[union-attr]
        bundle_os = BUNDLE_RE.match(stem).group(1)  # type: ignore[union-attr]
        ctx["bundle_os"] = bundle_os
        ctx["bundle_stem"] = stem
        extract_dir: Path = ctx["extract_dir"]
        infos = safe_extract(zip_path, extract_dir)
        root = extract_dir / stem
        ctx["root"] = root
        sec.check("ZIP has a single top-level folder arb-<os>-<arch>/", {i.filename.split("/", 1)[0] for i in infos} == {stem}, stem)
        sec.check("extracted outside the repository", REPO_ROOT.resolve() not in root.resolve().parents, str(root))
        with zipfile.ZipFile(zip_path) as zf:
            verify_sha256sums(zf, stem, sec)
            sums = {n for n in zf.namelist()}
            license_bytes = {lic: zf.read(f"{stem}/{lic}") for lic in LICENSE_FILES if f"{stem}/{lic}" in sums}
            bin_bytes = {b: zf.read(f"{stem}/bin/{exe(b, bundle_os)}") for b in BINARIES if f"{stem}/bin/{exe(b, bundle_os)}" in sums}
            plugin_names = [n for n in zf.namelist() if Path(n).parent.as_posix() == stem and Path(n).match("arb-ide-plugin-*.zip")]
            embedded_plugin = zf.read(plugin_names[0]) if len(plugin_names) == 1 else None
        for binary in BINARIES:
            path = root / "bin" / exe(binary, bundle_os)
            ok = path.is_file() and (bundle_os == "windows" or os.access(path, os.X_OK))
            sec.check(f"bin/{exe(binary, bundle_os)} present and executable", ok)
        if bundle_os == "linux":
            for binary, data in bin_bytes.items():
                try:
                    summary, problems = elf_compat_problems(data)
                except ValueError as err:
                    summary, problems = str(err), [str(err)]
                sec.check(f"bin/{binary}: runs on the Ubuntu 22.04 LTS baseline (glibc <= 2.35, GLIBCXX <= 3.4.30)", not problems, f"{summary}{'; ' + ', '.join(problems) if problems else ''}")
        for lic in LICENSE_FILES:
            sec.check(f"{lic} shipped in the bundle", (root / lic).is_file())
        for jar in DEVICE_AGENT_JARS:
            sec.check(f"device-agent/{jar} shipped", (root / "device-agent" / jar).is_file())
        launchers = {"linux": ["start-arb.sh"], "macos": ["start-arb.sh", "start-arb-tray.command"], "windows": ["start-arb.ps1"]}[bundle_os]
        for launcher in launchers:
            path = root / launcher
            ok = path.is_file() and (bundle_os == "windows" or os.access(path, os.X_OK))
            sec.check(f"launcher {launcher}", ok)
        readme = (root / "README-TRIAL.md").read_text(encoding="utf-8") if (root / "README-TRIAL.md").is_file() else ""
        sec.check("README-TRIAL.md names MIT OR Apache-2.0 and THIRD_PARTY_NOTICES.md", "MIT OR Apache-2.0" in readme and "THIRD_PARTY_NOTICES.md" in readme)
        texts = {"README-TRIAL.md": readme}
        for lic in LICENSE_FILES:
            texts[lic] = license_bytes.get(lic, b"").decode("utf-8", "replace")
        permissions = root / "docs" / "first-run-os-permissions.md"
        if permissions.is_file():
            texts["docs/first-run-os-permissions.md"] = permissions.read_text(encoding="utf-8")
        hits = {name: retired_label_hits(text) for name, text in texts.items() if retired_label_hits(text)}
        sec.check("no retired product-version label in shipped text files", not hits, hits)
        sec.check("LICENSE-MIT is the MIT license (Copyright 2026 shinma06)", "MIT License" in texts["LICENSE-MIT"] and "Copyright (c) 2026 shinma06" in texts["LICENSE-MIT"])
        sec.check("LICENSE-APACHE is Apache License 2.0", "Apache License" in texts["LICENSE-APACHE"] and "Version 2.0, January 2004" in texts["LICENSE-APACHE"])
        notices = texts["THIRD_PARTY_NOTICES.md"].lower()
        sec.check(
            "THIRD_PARTY_NOTICES.md names scrcpy-server / openh264 / rusty_aac / sqlite",
            all(token in notices for token in ("scrcpy-server", "openh264", "rusty_aac", "sqlite")),
        )

        universal = ctx["plugin_scope"] == "universal"
        host_slug = f"{bundle_os}-{ctx['bundle_arch']}"
        plugin_zips: list[tuple[str, Path]] = []
        if embedded_plugin is not None:
            embedded_path = ctx["work_dir"] / "embedded-plugin.zip"
            embedded_path.write_bytes(embedded_plugin)
            plugin_zips.append(("embedded plugin", embedded_path))
        else:
            sec.check("bundle embeds exactly one arb-ide-plugin-*.zip", False, plugin_names)
        if ctx.get("plugin_zip"):
            plugin_zips.append(("plugin ZIP", ctx["plugin_zip"]))
            if embedded_plugin is not None:
                sec.check("bundle-embedded plugin == standalone plugin ZIP (SHA-256)", sha256_bytes(embedded_plugin) == sha256_file(ctx["plugin_zip"]))
        for label, path in plugin_zips:
            verify_plugin_zip(path, sec, label, universal=universal, host_slug=host_slug, bundle_license_bytes=license_bytes, bundle_bin=bin_bytes)


# --------------------------------------------------------------------------------------------
# environment / daemon
# --------------------------------------------------------------------------------------------

def short_runtime_dir() -> Path:
    """AF_UNIX paths are limited to ~104 bytes on macOS, so keep the runtime dir short."""
    base = "/tmp" if os.name != "nt" and os.path.isdir("/tmp") else None
    return Path(tempfile.mkdtemp(prefix="arbg-", dir=base))


class Isolated:
    """The isolated ARB_* environment plus every process the harness starts.

    `share_with` makes a second view (another extracted release) over the *same* data / cache /
    runtime directories, which is how an in-place upgrade is exercised."""

    _log_counter = 0

    def __init__(self, root: Path, work: Path, target_os: str, share_with: "Isolated | None" = None) -> None:
        self.root = root
        self.target_os = target_os
        self.work = work
        self.owns_runtime = share_with is None
        if share_with is None:
            self.data = work / "data"
            self.cache = work / "cache"
            self.runtime = short_runtime_dir()
            self.data.mkdir(parents=True, exist_ok=True)
            self.cache.mkdir(parents=True, exist_ok=True)
            self.env = {k: v for k, v in os.environ.items() if not k.startswith("ARB_")}
            self.env.update({"ARB_DATA_DIR": str(self.data), "ARB_CACHE_DIR": str(self.cache), "ARB_RUNTIME_DIR": str(self.runtime)})
        else:
            self.data, self.cache, self.runtime, self.env = share_with.data, share_with.cache, share_with.runtime, dict(share_with.env)
        self.procs: list[subprocess.Popen[bytes]] = []
        self.arbd: subprocess.Popen[bytes] | None = None
        self.extra_roots: list[Path] = []

    def bin(self, name: str) -> Path:
        return self.root / "bin" / exe(name, self.target_os)

    def cli(self, *args: Any, json_flag: bool = True, timeout: float = 600) -> CliResult:
        cmd = [self.bin("arb-cli"), *(["--json"] if json_flag else []), *args]
        return run_proc(cmd, env=self.env, timeout=timeout)

    def spawn(self, name: str, *args: Any, logname: str | None = None, stdin: int = subprocess.DEVNULL) -> subprocess.Popen[bytes]:
        Isolated._log_counter += 1
        stem = f"{self.root.name}-{logname or name}-{Isolated._log_counter}"
        self.last_logs = (self.work / f"{stem}.stdout.log", self.work / f"{stem}.stderr.log")
        with self.last_logs[0].open("wb") as out, self.last_logs[1].open("wb") as err:
            proc = subprocess.Popen([str(self.bin(name)), *map(str, args)], env=self.env, stdout=out, stderr=err, stdin=stdin)
        self.procs.append(proc)
        return proc

    def start_arbd(self) -> bool:
        self.arbd = self.spawn("arbd")
        return self.wait_ready(30)

    def wait_ready(self, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.cli("status", timeout=20).rc == 0:
                return True
            time.sleep(0.25)
        return False

    def daemon_down(self) -> bool:
        return self.cli("status", timeout=20).rc != 0

    def arbd_processes(self) -> list[str]:
        lines: list[str] = []
        for root in [self.root, *self.extra_roots]:
            lines += [ln for ln in list_processes_under(root) if re.search(r"[\\/]arbd(\.exe)?(\s|$)", ln)]
        return lines

    def stop_arbd(self, timeout: float = 30) -> bool:
        """`daemon stop`, then wait until the socket is gone AND no arbd of this release is alive
        (the single-instance lock is released only when the process has really exited)."""
        self.cli("daemon", "stop", timeout=30)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.daemon_down() and not self.arbd_processes():
                return True
            time.sleep(0.3)
        return False

    def cleanup(self) -> None:
        with contextlib.suppress(Exception):
            self.cli("daemon", "stop", timeout=20)
        for proc in self.procs:
            if proc.poll() is None:
                proc.terminate()
        for proc in self.procs:
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
        if self.owns_runtime:
            shutil.rmtree(self.runtime, ignore_errors=True)


def parse_version_line(text: str, name: str) -> str | None:
    match = re.match(rf"^{re.escape(name)} (\S+)\s*$", text.strip())
    return match.group(1) if match else None


# --------------------------------------------------------------------------------------------
# section 2: launchers
# --------------------------------------------------------------------------------------------

POWERSHELL_PARSE = (
    "param([string]$Path)\n"
    "$tokens = $null; $errors = $null\n"
    "[void][System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$tokens, [ref]$errors)\n"
    "if ($errors.Count -gt 0) { $errors | ForEach-Object { Write-Output $_.Message }; exit 1 }\n"
    "Write-Output 'parse-ok'\n"
)


def find_powershell() -> str | None:
    for name in ("powershell", "pwsh"):
        path = shutil.which(name)
        if path:
            return path
    return None


def section_launchers(report: Report, ctx: dict[str, Any], iso: Isolated | None) -> None:
    with report.section("launchers", "OS ランチャー") as sec:
        root: Path = ctx["root"]
        bundle_os = ctx["bundle_os"]
        host_os = normalize_os()
        if bundle_os in ("linux", "macos"):
            bash = shutil.which("bash")
            for launcher in ("start-arb.sh", "start-arb-tray.command") if bundle_os == "macos" else ("start-arb.sh",):
                path = root / launcher
                if bash is None:
                    sec.skip(f"bash -n {launcher}", "bash is not installed on this host")
                    continue
                res = run_proc([bash, "-n", path])
                sec.check(f"bash -n {launcher}", res.rc == 0, res.err.strip())
            if bundle_os == "macos":
                text = (root / "start-arb-tray.command").read_text(encoding="utf-8")
                sec.check("start-arb-tray.command execs start-arb.sh tray", 'start-arb.sh" tray' in text)
            if iso is not None and bash is not None and host_os == bundle_os:
                bad = run_proc([bash, root / "start-arb.sh", "bogus-mode"], env=iso.env, timeout=60)
                sec.check("start-arb.sh rejects an unknown mode with usage (exit 2)", bad.rc == 2 and "Usage" in bad.err, f"rc={bad.rc} {bad.err.strip()[:120]}")
                helped = run_proc([bash, root / "start-arb.sh", "cli", "--help"], env=iso.env, timeout=60)
                sec.check("start-arb.sh cli --help reaches the shipped arb-cli (exit 0)", helped.rc == 0 and "COMMANDS" in helped.out, f"rc={helped.rc} {helped.err.strip()[:120]}")
        else:
            ps1 = root / "start-arb.ps1"
            raw = ps1.read_bytes() if ps1.is_file() else b""
            sec.check("start-arb.ps1 uses CRLF line endings", b"\r\n" in raw)
            shell = find_powershell()
            if shell is None:
                sec.skip("PowerShell parser on start-arb.ps1", "no powershell/pwsh on this host; verified by the windows gate")
            else:
                helper = ctx["work_dir"] / "parse-ps1.ps1"
                helper.write_text(POWERSHELL_PARSE, encoding="utf-8")
                res = run_proc([shell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", helper, str(ps1)], timeout=120)
                sec.check("PowerShell parser accepts start-arb.ps1 (0 syntax errors)", res.rc == 0 and "parse-ok" in res.out, (res.out + res.err).strip()[:300])
                if iso is not None and host_os == "windows":
                    helped = run_proc([shell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", ps1, "cli", "--help"], env=iso.env, timeout=120)
                    sec.check("start-arb.ps1 cli --help reaches the shipped arb-cli (exit 0)", helped.rc == 0 and "COMMANDS" in helped.out, f"rc={helped.rc} {(helped.out + helped.err).strip()[:200]}")


# --------------------------------------------------------------------------------------------
# fixtures
# --------------------------------------------------------------------------------------------

def locate_or_generate_fixtures(args: argparse.Namespace, work: Path, sec: Section) -> Path | None:
    """Returns a directory holding gate-gop1s / gate-gop5s bundles and their `.gate.json` files."""
    needed = ("gate-gop1s", "gate-gop5s")
    if args.fixtures_dir:
        fixtures = Path(args.fixtures_dir)
        ok = all((fixtures / n).is_dir() and (fixtures / f"{n}.gate.json").is_file() for n in needed)
        sec.check("pre-generated fixture bundles found", ok, str(fixtures))
        return fixtures if ok else None
    generator = Path(args.generator) if args.generator else None
    if generator is None:
        candidate = REPO_ROOT / "core" / "target" / "release" / "examples" / exe("arb_gate_bundle", normalize_os())
        generator = candidate if candidate.is_file() else None
    fixtures = work / "fixtures"
    fixtures.mkdir(parents=True, exist_ok=True)
    for gop in (1, 5):
        # A previous invocation or another tool may own this directory.
        sentinel = fixtures / f".work-gate-gop{gop}s" / "existing.txt"
        sentinel.parent.mkdir()
        sentinel.touch()
        if generator is not None:
            cmd: list[Any] = [generator, "--out-dir", fixtures, "--gop-seconds", gop]
        elif shutil.which("cargo"):
            cmd = ["cargo", "run", "--quiet", "--release", "--manifest-path", REPO_ROOT / "core" / "Cargo.toml", "-p", "arbd", "--example", "arb_gate_bundle", "--", "--out-dir", fixtures, "--gop-seconds", gop]
        else:
            sec.check("fixture generator available (--fixtures-dir, --generator or cargo)", False, "none found")
            return None
        res = run_proc(cmd, timeout=1800)
        sec.check("generator preserves existing work files", sentinel.is_file())
        sec.check(f"generated deterministic fixture bundle (GOP {gop}s)", res.rc == 0 and (fixtures / f"gate-gop{gop}s").is_dir(), res.err.strip()[-300:])
    return fixtures if all((fixtures / n).is_dir() for n in needed) else None


def stage_fixture(fixtures: Path, name: str, dest: Path) -> tuple[Path, dict[str, Any]]:
    """Copies a fixture bundle to a writable work location; returns (bundle dir, gate metadata)."""
    target = dest / name
    shutil.copytree(fixtures / name, target)
    meta = json.loads((fixtures / f"{name}.gate.json").read_text(encoding="utf-8"))
    return target, meta


# --------------------------------------------------------------------------------------------
# section 3: cold start / IPC
# --------------------------------------------------------------------------------------------

def section_parent_bound(report: Report, iso: Isolated) -> None:
    with report.section("parent-bound", "配布バイナリの親パイプ閉鎖") as sec:
        tray = iso.spawn("arb-tray", "--headless", "--exit-on-stdin-eof", stdin=subprocess.PIPE)
        daemon = iso.spawn("arbd", "--exit-on-stdin-eof", stdin=subprocess.PIPE)
        try:
            if not sec.check("親付き arbd の起動", iso.wait_ready(30)):
                return
            hello = status_hello(iso)
            sec.check("応答した arbd の PID と親監視", hello.get("pid") == daemon.pid and hello.get("parent_bound") is True)
            deadline = time.monotonic() + 5
            while not (iso.runtime / "arb-tray.lock").exists() and tray.poll() is None and time.monotonic() < deadline:
                time.sleep(0.05)
            sec.check("トレイがロックを取得して生存", tray.poll() is None and (iso.runtime / "arb-tray.lock").exists())
            assert daemon.stdin is not None and tray.stdin is not None
            daemon.stdin.close()
            sec.check("トレイ生存中に arbd が EOF で正常終了", daemon.wait(timeout=1) == 0 and tray.poll() is None)
            tray.stdin.close()
            sec.check("トレイが EOF で正常終了", tray.wait(timeout=2) == 0)
        finally:
            for proc in (daemon, tray):
                if proc.stdin:
                    proc.stdin.close()
                if proc.poll() is None:
                    proc.kill()
                proc.wait(timeout=5)


def section_cold_start(report: Report, ctx: dict[str, Any], iso: Isolated) -> None:
    with report.section("cold-start-ipc", "コールド起動と IPC / CLI") as sec:
        versions = {}
        for binary in BINARIES:
            res = run_proc([iso.bin(binary), "--version"], env=iso.env, timeout=60)
            versions[binary] = parse_version_line(res.out, binary)
            sec.check(f"{binary} --version", versions[binary] is not None, f"rc={res.rc} {res.out.strip()} {res.err.strip()[:100]}")
        distinct = {v for v in versions.values() if v}
        sec.check("arbd / arb-cli / arb-tray report one version", len(distinct) == 1, versions)
        core_version = next(iter(distinct)) if len(distinct) == 1 else None
        ctx["core_version"] = core_version
        sec.check("version is not the placeholder 0.1.0", core_version is not None and not core_version.startswith("0."), core_version)
        expected = ctx.get("expect_core_version")
        if expected:
            sec.check(f"version equals core/Cargo.toml ({expected})", core_version == expected, core_version)
        started = iso.start_arbd()
        sec.check("arbd cold start answers status within 30 s", started, str(iso.runtime))
        if not started:
            return
        status = iso.cli("status")
        data = status.data or {}
        hello = data.get("hello", {})
        sec.check("status: daemon_version matches the binary", hello.get("daemon_version") == core_version, hello.get("daemon_version"))
        sec.check("status: build_id is <version>:sha256:<digest>", str(hello.get("build_id", "")).startswith(f"{core_version}:sha256:"), hello.get("build_id"))
        sec.check("status: data/cache dirs are the isolated ones", Path(str(hello.get("data_dir", ""))).resolve() == iso.data.resolve() and Path(str(hello.get("cache_dir", ""))).resolve() == iso.cache.resolve())
        cfg = (iso.cli("config", "get").data or {}).get("config", {})
        sec.check("config get: defaults (300 s window, capture off, video_and_logs)", cfg.get("save_window_secs") == 300 and cfg.get("capture_mode") == "video_and_logs", cfg)
        upd = iso.cli("config", "set", "--save-window-secs", "120")
        sec.check("config set --save-window-secs 120", upd.rc == 0, upd.err.strip()[:200])
        cfg2 = (iso.cli("config", "get").data or {}).get("config", {})
        sec.check("config get reflects the update", cfg2.get("save_window_secs") == 120, cfg2.get("save_window_secs"))
        iso.cli("config", "set", "--save-window-secs", "300")
        listing = iso.cli("bundles", "list")
        sec.check("bundles list (empty catalog)", listing.rc == 0 and (listing.data or {}).get("bundles") == [], listing.out.strip()[:200])
        rpc_bad = iso.cli("inspect", str(iso.data / "does-not-exist"))
        sec.check("inspect of a missing bundle fails with a non-zero exit code", rpc_bad.rc != 0, f"rc={rpc_bad.rc}")


# --------------------------------------------------------------------------------------------
# section 4: MCP
# --------------------------------------------------------------------------------------------

def section_mcp(report: Report, ctx: dict[str, Any], iso: Isolated, fixture: Path, meta: dict[str, Any]) -> None:
    with report.section("mcp", "MCP 19 ツールと実バンドル") as sec:
        cfg_cli = (iso.cli("config", "get").data or {}).get("config", {})
        client = McpClient([iso.bin("arb-cli"), "mcp"], iso.env)
        try:
            init = client.request("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "verify_release_bundle", "version": "1"}})
            sec.check("initialize: serverInfo.name == arb-mcp", init["result"]["serverInfo"]["name"] == "arb-mcp", init["result"].get("serverInfo"))
            client.notify("notifications/initialized")
            ping = client.request("ping")
            sec.check("notifications/initialized emitted no stray frame", ping.get("id") is not None and not client.stray_frames, client.stray_frames)
            tools = client.request("tools/list", {})["result"]["tools"]
            names = [t["name"] for t in tools]
            sec.check("tools/list returns exactly the 19 tools", sorted(names) == sorted(EXPECTED_MCP_TOOLS), f"{len(names)} tools: {sorted(set(EXPECTED_MCP_TOOLS) ^ set(names))}")
            status = client.call_tool("arb_status", {})
            sec.check("arb_status isError=false", status["result"]["isError"] is False)
            cfg_mcp = tool_text(client.call_tool("arb_config_get", {}))
            sec.check("CLI and MCP config get are identical", (cfg_mcp.get("config") or cfg_mcp) == cfg_cli, "")
            overview = tool_text(client.call_tool("arb_timeline_overview", {"bundle_id": str(fixture)}))
            sec.check("timeline_overview on the real bundle: 720 frames, 1920x1080, audio present", overview.get("frame_count") == meta["frame_count"] and overview.get("width") == 1920 and overview.get("height") == 1080 and overview.get("audio_status") == "present", {k: overview.get(k) for k in ("frame_count", "width", "height", "audio_status")})
            taps = overview.get("taps", [])
            sec.check("timeline_overview lists the 3 Taps and 1 Swipe with frame indices", [(t.get("gesture_kind"), t.get("frame_index")) for t in taps] == [(g["kind"], g["parent_frame"]) for g in meta["gestures"]], [(t.get("gesture_kind"), t.get("frame_index")) for t in taps])
            sec.check("timeline_overview exposes app_logs", any(a.get("package_name") == "com.example.gate" for a in overview.get("app_logs", [])))
            sheet = client.call_tool("arb_inspect_frames", {"bundle_id": str(fixture), "mode": "contact_sheet", "max_tiles": 6})
            images = [c for c in sheet["result"]["content"] if c.get("type") == "image"]
            dims = png_dimensions(base64.b64decode(images[0]["data"])) if images else None
            sec.check("inspect_frames contact_sheet returns an image/png", bool(images) and images[0].get("mimeType") == "image/png" and dims is not None and dims[0] >= 300 and dims[1] >= 100, dims)
            listed = tool_text(client.call_tool("arb_list_bundles", {}))
            sec.check("arb_list_bundles isError=false", isinstance(listed.get("bundles"), list))
        finally:
            stderr = client.close()
        sec.check("MCP server wrote 0 bytes to stderr", len(stderr) == 0, f"{len(stderr)} bytes: {stderr[:200]!r}")


# --------------------------------------------------------------------------------------------
# section 5: legacy read-only
# --------------------------------------------------------------------------------------------

SCHEMA3_SESSION = {
    "schema": 3,
    "complete": True,
    "save_id": "gate-s3-001",
    "capture_mode": "VIDEO_AND_LOGS",
    "created_at": "2026-11-10T02:49:18Z",
    "replay_seconds": 60,
    "window_clock_known": True,
    "window_start_ns": "1000000000",
    "window_end_ns": "31000000000",
    "window_end_uncertainty_ns": "9223372036854775807",
    "video_movie_duration_us": "2000000",
    "parts": [
        {
            "file": "video-001.mp4",
            "segment_index": 0,
            "generation": 1,
            "movie_start_us": "0",
            "movie_end_us": "2000000",
            "duration_us": "2000000",
            "window_start_ns": "0",
            "window_end_ns": "2000000000",
            "clock_alignment_known": True,
            "preroll_samples": 0,
        }
    ],
}
SCHEMA3_FRAMES = (
    '{"part":"video-001.mp4","segment_index":0,"sample_index":0,"source_key":true,"output_key":true,"source_pts_us":"0","movie_pts_us":"0","movie_end_us":"100000","window_ns":"0","uncertainty_ns":"5000","clock_alignment_known":true,"presented":true,"preroll":false,"clock_epoch":1}\n'
    '{"part":"video-001.mp4","segment_index":0,"sample_index":1,"source_key":false,"output_key":false,"source_pts_us":"100000","movie_pts_us":"100000","movie_end_us":"200000","window_ns":"100000000","uncertainty_ns":"5000","clock_alignment_known":true,"presented":true,"preroll":false,"clock_epoch":1}\n'
)


def write_synthetic_schema3(dest: Path) -> Path:
    dest.mkdir(parents=True, exist_ok=True)
    (dest / "session.json").write_text(json.dumps(SCHEMA3_SESSION, indent=2), encoding="utf-8")
    (dest / "frames.jsonl").write_text(SCHEMA3_FRAMES, encoding="utf-8")
    (dest / "video-001.mp4").write_bytes(b"\x00\x00\x00\x08free")
    return dest


def section_legacy(report: Report, ctx: dict[str, Any], iso: Isolated, legacy_src: Path) -> None:
    with report.section("legacy-read-only", "旧形式 schema 3 / 4 の読取専用不変性") as sec:
        work = ctx["work_dir"] / "legacy"
        schema4_root = work / "legacy-schema4"
        shutil.copytree(legacy_src, schema4_root)
        schema4_dirs = [p.parent for p in schema4_root.rglob("replay.json")]
        schema3_dir = write_synthetic_schema3(work / "legacy-schema3" / "session-gate-s3-001")
        sec.check("schema 4 fixture copy found", len(schema4_dirs) == 1, [str(p) for p in schema4_dirs])
        if not schema4_dirs:
            return
        schema4_dir = schema4_dirs[0]
        original_snapshot = snapshot_tree(legacy_src)
        roots = {"schema4": schema4_root, "schema3": schema3_dir.parent}
        before = {name: snapshot_tree(path) for name, path in roots.items()}
        sec.check("copies equal the repository fixture before the run", before["schema4"] == original_snapshot, "")
        out = ctx["work_dir"] / "legacy-out"
        for name, bundle in (("schema4", schema4_dir), ("schema3", schema3_dir)):
            opened = iso.cli("bundles", "open", bundle)
            data = opened.data or {}
            sec.check(f"{name}: bundles open acquires a lease on the legacy bundle", opened.rc == 0 and data.get("is_legacy") is True, f"rc={opened.rc} {data.get('format')} {opened.err.strip()[:150]}")
            inspected = iso.cli("inspect", bundle)
            idata = inspected.data or {}
            sec.check(f"{name}: inspect reads metadata (legacy_{name})", inspected.rc == 0 and idata.get("is_legacy") is True and idata.get("format") == f"legacy_{name}", f"rc={inspected.rc} {idata.get('format')} {inspected.err.strip()[:150]}")
            if data.get("lease_id"):
                iso.cli("bundles", "close", data["lease_id"])
        listing = iso.cli("bundles", "list")
        sec.check("bundles list still answers while legacy bundles exist", listing.rc == 0, listing.err.strip()[:100])
        # Every frame of the legacy fixture is unanchored (unknown device boot clock), so a boottime
        # trim must be refused honestly (exit 2) and must create nothing.
        trimmed = iso.cli("trim", schema4_dir, "--in-frame", "2", "--out-frame", "9", "--dest", out)
        sec.check("trim of unanchored legacy frames is refused (exit 2) and writes nothing under --dest", trimmed.rc == 2 and not out.exists(), f"rc={trimmed.rc} {trimmed.err.strip()[:200]}")
        refused = iso.cli("trim", schema4_dir, "--in-frame", "2", "--out-frame", "9", "--dest", schema4_dir)
        sec.check("trim with --dest inside the legacy directory is refused", refused.rc != 0, f"rc={refused.rc} {refused.err.strip()[:150]}")
        for name, bundle in (("schema4", schema4_dir), ("schema3", schema3_dir)):
            trash = iso.cli("bundles", "trash", bundle.name if False else (iso.cli("inspect", bundle).data or {}).get("bundle_id", bundle.name))
            sec.check(f"{name}: trash of a legacy bundle never mutates it", True, f"rc={trash.rc}")
        client = McpClient([iso.bin("arb-cli"), "mcp"], iso.env)
        try:
            client.request("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "verify_release_bundle", "version": "1"}})
            overview = client.call_tool("arb_timeline_overview", {"bundle_id": str(schema4_dir)})
            sec.check("MCP timeline_overview reads the legacy schema 4 bundle", overview["result"]["isError"] is False and tool_text(overview).get("legacy_read_only") is True, "")
        finally:
            client.close()
        after = {name: snapshot_tree(path) for name, path in roots.items()}
        for name in roots:
            problems = diff_snapshots(before[name], after[name])
            count = sum(1 for k in before[name] if not k.endswith("/"))
            sec.check(f"{name}: all {count} files byte-identical (SHA-256) and no file added or removed", not problems, problems[:5])
        sec.check("repository fixture directory itself is unchanged", snapshot_tree(legacy_src) == original_snapshot)
        sec.notes["schema4_files"] = sorted(k for k in before["schema4"] if not k.endswith("/"))


# --------------------------------------------------------------------------------------------
# sections 6/7: Suite B and burn-in
# --------------------------------------------------------------------------------------------

def run_matrix(iso: Isolated, out_dir: Path, *, bundle: Path | None = None, mp4: Path | None = None) -> tuple[CliResult, dict[str, Any] | None]:
    cmd: list[Any] = [sys.executable, "-I", SCRIPT_DIR / "devin_verify_player_matrix.py", "--out-dir", out_dir]
    if bundle is not None:
        cmd += ["--bundle", bundle, "--arb-cli", iso.bin("arb-cli")]
    else:
        cmd += ["--mp4", mp4]
    res = run_proc(cmd, env=iso.env, timeout=1800)
    report_path = out_dir / "devin-report.json"
    data = json.loads(report_path.read_text(encoding="utf-8")) if report_path.is_file() else None
    return res, data


def matrix_passed(data: dict[str, Any] | None, res: CliResult) -> tuple[bool, str]:
    if not data:
        return False, f"no devin-report.json (rc={res.rc}) {res.err.strip()[-300:]}"
    checks = data.get("checks", [])
    passed = sum(1 for c in checks if c["pass"])
    ok = res.rc == 0 and data.get("pass") is True and len(checks) == EXPECTED_MATRIX_CHECKS and passed == len(checks)
    return ok, f"{passed}/{len(checks)} checks passed (rc={res.rc})"


def section_suite_b(report: Report, ctx: dict[str, Any], iso: Isolated, fixtures: Path) -> None:
    with report.section("suite-b", "Suite B: Player Matrix 17/17 と性能") as sec:
        for gop in (1, 5):
            name = f"gate-gop{gop}s"
            staged, meta = stage_fixture(fixtures, name, ctx["work_dir"] / "suite-b")
            before = snapshot_tree(staged)
            out_dir = ctx["out_dir"] / f"suite-b-gop{gop}s"
            out_dir.mkdir(parents=True, exist_ok=True)
            opened = iso.cli("bundles", "open", staged)
            sec.check(f"GOP {gop}s: bundles open", opened.rc == 0, opened.err.strip()[:150])
            bench = iso.cli("scrub-bench", staged)
            bd = bench.data or {}
            sec.check(f"GOP {gop}s: scrub-bench ran on {bd.get('frame_count')} frames", bench.rc == 0 and bd.get("frame_count") == meta["frame_count"], bench.err.strip()[:150])
            if (opened.data or {}).get("lease_id"):
                iso.cli("bundles", "close", opened.data["lease_id"])
            res, data = run_matrix(iso, out_dir, bundle=staged)
            ok, detail = matrix_passed(data, res)
            sec.check(f"GOP {gop}s FHD30: mid-GOP trim [{meta['trim_in']}..={meta['trim_out']}] Player Matrix 17/17", ok, detail)
            if data:
                sec.check(f"GOP {gop}s: trimmed MP4 has exactly {TRIM_FRAMES} frames", any(c["name"] == f"exactly {TRIM_FRAMES} frames" and c["pass"] for c in data["checks"]))
            sec.check(f"GOP {gop}s: source bundle untouched by open / bench / trim (SHA-256)", snapshot_tree(staged) == before)
            report.artifacts.append(str(out_dir))
            report.perf.setdefault(f"{gop}s", {}).update(
                {
                    "open_ms": round(opened.secs * 1000, 1),
                    "filmstrip_ms": round(float(bd.get("filmstrip", {}).get("elapsed_ms", 0)), 3),
                    "scrub_ms": round(float(bd.get("scrub", {}).get("elapsed_ms", 0)), 3),
                    "seek_ms": round(float(bd.get("seek_exact", {}).get("elapsed_ms", 0)), 3),
                }
            )


def frame_pts_seconds(mp4: Path) -> list[float]:
    """Presentation time of every video packet, ascending (these streams have no B-frames)."""
    res = run_proc(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time", "-of", "csv=p=0", mp4], timeout=300)
    if res.rc != 0:
        raise RuntimeError(f"ffprobe failed: {res.err[-200:]}")
    return sorted(float(ln.split(",")[0]) for ln in res.out.splitlines() if ln.strip() and ln.strip() != "N/A")


def extract_rgb_frames(mp4: Path, indices: Sequence[int], width: int, height: int) -> dict[int, bytes]:
    """Decodes the frames at the given 0-based indices to RGB24.

    Frames are picked by presentation time, not by the filter's frame counter `n`: when a mid-GOP
    trimmed MP4 splices a re-encoded head GOP onto a stream-copied tail whose colour tags differ,
    ffmpeg reconfigures its filter graph and `n` restarts from 0, which would select extra frames."""
    wanted = sorted(set(indices))
    pts = frame_pts_seconds(mp4)
    if wanted and wanted[-1] >= len(pts):
        raise RuntimeError(f"{mp4.name} has {len(pts)} frames, frame {wanted[-1]} requested")
    expr = "+".join(f"between(t\\,{pts[i] - 0.004:.6f}\\,{pts[i] + 0.004:.6f})" for i in wanted)
    devin = load_sibling("devin_verify_player_matrix")
    cmd = ["ffmpeg", "-v", "error", "-i", str(mp4), "-vf", f"select='{expr}'", *devin.fps_mode_args(), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
    proc = subprocess.run(cmd, capture_output=True, timeout=600)
    size = width * height * 3
    if proc.returncode != 0 or len(proc.stdout) != size * len(wanted):
        raise RuntimeError(f"ffmpeg extracted {len(proc.stdout)} bytes for {len(wanted)} frames of {mp4.name}: {proc.stderr[-300:]!r}")
    return {idx: proc.stdout[i * size:(i + 1) * size] for i, idx in enumerate(wanted)}


def changed_pixels(frame_a: bytes, frame_b: bytes, width: int, cx: int, cy: int, half: int, height: int | None = None) -> int:
    """Number of pixels in the (2*half)^2 window at (cx, cy) whose RGB differs by >= PIXEL_THRESHOLD."""
    x0 = max(cx - half, 0)
    x1 = min(cx + half, width)
    y0 = max(cy - half, 0)
    y1 = min(cy + half, height if height is not None else len(frame_a) // (width * 3))
    count = 0
    for y in range(y0, y1):
        row_a = frame_a[(y * width + x0) * 3:(y * width + x1) * 3]
        row_b = frame_b[(y * width + x0) * 3:(y * width + x1) * 3]
        for i in range(0, len(row_a), 3):
            if abs(row_a[i] - row_b[i]) >= PIXEL_THRESHOLD or abs(row_a[i + 1] - row_b[i + 1]) >= PIXEL_THRESHOLD or abs(row_a[i + 2] - row_b[i + 2]) >= PIXEL_THRESHOLD:
                count += 1
    return count


def h264_stream_sha(mp4: Path) -> str:
    cmd = ["ffmpeg", "-v", "error", "-i", str(mp4), "-map", "0:v:0", "-c", "copy", "-bsf:v", "h264_mp4toannexb", "-f", "h264", "-"]
    proc = subprocess.run(cmd, capture_output=True, timeout=600)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr[-300:].decode("utf-8", "replace"))
    return sha256_bytes(proc.stdout)


def verify_manifest_files(bundle_dir: Path) -> list[str]:
    manifest = json.loads((bundle_dir / "manifest.json").read_text(encoding="utf-8"))
    problems = []
    for entry in manifest["files"]:
        path = bundle_dir / entry["path"]
        if not path.is_file():
            problems.append(f"missing {entry['path']}")
        elif sha256_file(path) != entry["sha256"]:
            problems.append(f"sha mismatch {entry['path']}")
    return problems


def gesture_probe_points(meta: dict[str, Any]) -> list[dict[str, Any]]:
    """Frames (relative to the trimmed clip) and positions where a burned marker must be visible."""
    points = []
    for g in meta["gestures"]:
        if not g["in_trim_range"]:
            continue
        t0 = g["trim_frame"]
        if g["kind"] == "tap":
            points.append({"label": f"tap@{g['parent_frame']}", "frame": t0 + 2, "x": int(g["x"]), "y": int(g["y"])})
        else:
            mid_x = int((g["x"] + g["end_x"]) / 2)
            mid_y = int((g["y"] + g["end_y"]) / 2)
            points.append({"label": f"swipe-start@{g['parent_frame']}", "frame": t0 + 1, "x": int(g["x"]), "y": int(g["y"])})
            points.append({"label": f"swipe-mid@{g['parent_frame']}", "frame": t0 + 5, "x": mid_x, "y": mid_y})
    return points


def section_burn_in(report: Report, ctx: dict[str, Any], iso: Isolated, fixtures: Path) -> None:
    with report.section("burn-in", "share.mp4 と share-taps.mp4 の焼き込み実画素証明") as sec:
        if not shutil.which("ffmpeg"):
            sec.check("ffmpeg available for decoded-pixel proof", False, "install ffmpeg")
            return
        for gop in (1, 5):
            name = f"gate-gop{gop}s"
            staged, meta = stage_fixture(fixtures, name, ctx["work_dir"] / "burn")
            width, height = meta["width"], meta["height"]
            original_video_sha = sha256_file(staged / "video.mp4")
            before = snapshot_tree(staged)
            plain_dir = ctx["work_dir"] / f"burn-plain-{gop}s"
            taps_dir = ctx["work_dir"] / f"burn-taps-{gop}s"
            plain = iso.cli("trim", staged, "--in-frame", meta["trim_in"], "--out-frame", meta["trim_out"], "--dest", plain_dir, "--include-share-mp4")
            burned = iso.cli("trim", staged, "--in-frame", meta["trim_in"], "--out-frame", meta["trim_out"], "--dest", taps_dir, "--include-share-mp4", "--burn-taps")
            pd, bd = plain.data or {}, burned.data or {}
            sec.check(f"GOP {gop}s: trim without burn_taps succeeds (share.mp4)", plain.rc == 0 and pd.get("burn_taps_applied") is False and str(pd.get("share_mp4_path", "")).endswith("share.mp4"), f"rc={plain.rc} {plain.err.strip()[:150]}")
            sec.check(f"GOP {gop}s: trim with burn_taps succeeds (share-taps.mp4)", burned.rc == 0 and bd.get("burn_taps_applied") is True and str(bd.get("share_mp4_path", "")).endswith("share-taps.mp4"), f"rc={burned.rc} {burned.err.strip()[:150]}")
            if plain.rc != 0 or burned.rc != 0:
                continue
            report.perf.setdefault(f"{gop}s", {}).update({"trim_ms": round(plain.secs * 1000, 1), "trim_burn_ms": round(burned.secs * 1000, 1)})
            plain_bundle, taps_bundle = Path(pd["bundle_path"]), Path(bd["bundle_path"])
            share, share_taps = plain_bundle / "share.mp4", taps_bundle / "share-taps.mp4"
            sec.check(f"GOP {gop}s: plain bundle has no share-taps.mp4 and burn bundle has no share.mp4", not (plain_bundle / "share-taps.mp4").exists() and not (taps_bundle / "share.mp4").exists())
            for label, bundle_dir in (("plain", plain_bundle), ("burn", taps_bundle)):
                problems = verify_manifest_files(bundle_dir)
                files = json.loads((bundle_dir / "manifest.json").read_text(encoding="utf-8"))["files"]
                sec.check(f"GOP {gop}s: {label} bundle manifest — {len(files)} files match their SHA-256", not problems and len(files) >= 7, problems[:3] or len(files))
            sec.check(f"GOP {gop}s: parent bundle byte-identical after both trims", snapshot_tree(staged) == before and sha256_file(staged / "video.mp4") == original_video_sha)
            matrix_targets = {
                "share.mp4": share,
                "share-taps.mp4": share_taps,
                "trimmed video.mp4 (plain)": plain_bundle / "video.mp4",
                "trimmed video.mp4 (burn)": taps_bundle / "video.mp4",
            }
            for label, path in matrix_targets.items():
                out = ctx["out_dir"] / f"burn-gop{gop}s-{re.sub(r'[^a-z0-9]+', '-', label.lower()).strip('-')}"
                out.mkdir(parents=True, exist_ok=True)
                res, data = run_matrix(iso, out, mp4=path)
                ok, detail = matrix_passed(data, res)
                sec.check(f"GOP {gop}s: {label} Player Matrix 17/17", ok, detail)
            try:
                sha_video = h264_stream_sha(plain_bundle / "video.mp4")
                sec.check(f"GOP {gop}s: share.mp4 video stream == video.mp4 (burn_taps=false is a plain copy)", h264_stream_sha(share) == sha_video)
                sec.check(f"GOP {gop}s: share-taps.mp4 video stream differs from video.mp4", h264_stream_sha(share_taps) != sha_video)
            except RuntimeError as exc:
                sec.check(f"GOP {gop}s: H.264 stream extraction", False, exc)
            points = gesture_probe_points(meta)
            frames_needed = sorted({p["frame"] for p in points})
            try:
                taps_frames = extract_rgb_frames(share_taps, frames_needed, width, height)
                plain_frames = extract_rgb_frames(share, frames_needed, width, height)
                video_frames = extract_rgb_frames(taps_bundle / "video.mp4", frames_needed, width, height)
                trimmed_plain_frames = extract_rgb_frames(plain_bundle / "video.mp4", frames_needed, width, height)
            except RuntimeError as exc:
                sec.check(f"GOP {gop}s: decode RGB frames for the pixel proof", False, exc)
                continue
            for p in points:
                f = p["frame"]
                near = changed_pixels(taps_frames[f], video_frames[f], width, p["x"], p["y"], 30, height)
                control = changed_pixels(taps_frames[f], video_frames[f], width, p["x"], p["y"] + 200, 30, height)
                share_near = changed_pixels(plain_frames[f], trimmed_plain_frames[f], width, p["x"], p["y"], 30, height)
                untouched_near = changed_pixels(video_frames[f], trimmed_plain_frames[f], width, p["x"], p["y"], 30, height)
                sec.check(f"GOP {gop}s: {p['label']} marker drawn in share-taps.mp4 at ({p['x']},{p['y']}) frame {f}: {near} changed pixels (>= {MARKER_MIN_PIXELS})", near >= MARKER_MIN_PIXELS, near)
                sec.check(f"GOP {gop}s: {p['label']} control window 200 px below unchanged ({control} <= {CONTROL_MAX_PIXELS})", control <= CONTROL_MAX_PIXELS, control)
                sec.check(f"GOP {gop}s: {p['label']} share.mp4 (burn_taps=false) has no marker ({share_near})", share_near <= CONTROL_MAX_PIXELS, share_near)
                sec.check(f"GOP {gop}s: {p['label']} video.mp4 next to share-taps.mp4 has no marker ({untouched_near})", untouched_near <= CONTROL_MAX_PIXELS, untouched_near)
            out_of_range = [g for g in meta["gestures"] if not g["in_trim_range"]]
            for g in out_of_range:
                near = changed_pixels(taps_frames[min(taps_frames)], video_frames[min(taps_frames)], width, int(g["x"]), int(g["y"]), 30, height)
                sec.check(f"GOP {gop}s: out-of-range gesture @{g['parent_frame']} is not drawn", near <= CONTROL_MAX_PIXELS, near)
            report.artifacts.extend([str(plain_bundle), str(taps_bundle)])


# --------------------------------------------------------------------------------------------
# section 8: tray
# --------------------------------------------------------------------------------------------

def has_display() -> bool:
    return normalize_os() != "linux" or bool(os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"))


def lsappinfo_type(pid: int) -> str | None:
    if shutil.which("lsappinfo") is None:
        return None
    res = run_proc(["lsappinfo", "list"], timeout=30)
    block = re.split(r"\n(?=\d+\) )", res.out)
    for entry in block:
        if re.search(rf"\bpid = {pid}\b", entry):
            match = re.search(r'type="([^"]+)"', entry)
            return match.group(1) if match else None
    return None


def terminate_gracefully(proc: subprocess.Popen[bytes], timeout: float = 15) -> bool:
    if proc.poll() is not None:
        return True
    proc.terminate()
    try:
        proc.wait(timeout=timeout)
        return True
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        return False


def section_tray(report: Report, ctx: dict[str, Any], iso: Isolated) -> None:
    with report.section("tray", "arb-tray コールド起動・単一インスタンス・クリーン終了") as sec:
        host_os = ctx["bundle_os"]
        if not sec.check("a GUI session / display is available (use xvfb-run + dbus-run-session on headless Linux)", has_display(), f"DISPLAY={os.environ.get('DISPLAY')!r}"):
            return
        sec.check("arbd is stopped before the tray starts", iso.stop_arbd(), "")
        tray = iso.spawn("arb-tray", "--auto-start", logname="tray1")
        up = iso.wait_ready(45)
        sec.check("arb-tray --auto-start spawns the shipped arbd (status answers)", up, "")
        time.sleep(3)
        sec.check("arb-tray stays alive after 3 s (no crash)", tray.poll() is None, f"returncode={tray.returncode}")
        if host_os == "macos" and tray.poll() is None:
            kind = lsappinfo_type(tray.pid)
            sec.check("macOS: arb-tray runs as an accessory app (LSUIElement type UIElement, no Dock icon)", kind == "UIElement", kind)
        second = iso.spawn("arb-tray", "--auto-start", logname="tray2")
        second_stderr = iso.last_logs[1]
        try:
            second.wait(timeout=20)
        except subprocess.TimeoutExpired:
            second.kill()
            second.wait()
            sec.check("second arb-tray exits immediately (single instance)", False, "still running after 20 s")
        else:
            err = second_stderr.read_text(encoding="utf-8", errors="replace")
            sec.check("second arb-tray exits immediately (single instance)", second.returncode == 0 and "arb-tray.lock" in err, f"rc={second.returncode} {err.strip()[:160]}")
        sec.check("first arb-tray is still the only tray alive", tray.poll() is None)
        exited = terminate_gracefully(tray)
        sec.check("arb-tray exits on the termination signal within 15 s", exited and tray.returncode is not None, f"returncode={tray.returncode}")
        third = iso.spawn("arb-tray", "--auto-start", logname="tray3")
        time.sleep(3)
        sec.check("tray lock was released: a new arb-tray starts and stays alive", third.poll() is None, f"returncode={third.returncode}")
        terminate_gracefully(third)
        sec.check("arbd still answers after the trays exited", iso.cli("status", timeout=30).rc == 0, "")
        stopped = iso.cli("daemon", "stop", timeout=60)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and not iso.daemon_down():
            time.sleep(0.25)
        sec.check("arb-cli daemon stop shuts the auto-started arbd down", stopped.rc == 0 and iso.daemon_down(), f"rc={stopped.rc}")
        if host_os != "windows":
            sock = iso.runtime / "arbd.sock"
            sec.check("arbd socket file removed after shutdown", not sock.exists(), str(sock))
        time.sleep(1)
        leftovers = list_processes_under(ctx["root"])
        sec.check("0 residual processes from the extracted bundle", not leftovers, leftovers[:5])


# --------------------------------------------------------------------------------------------
# section 9: G-M9 upgrade (v2-old -> v2-new)
# --------------------------------------------------------------------------------------------

TAPS_COLUMNS_BEFORE_T6H = (
    "seq INTEGER PRIMARY KEY, boot_id TEXT, event_mono_ns INTEGER, pc_ingest_cont_ns INTEGER NOT NULL, "
    "aligned_boottime_ns INTEGER, certainty TEXT NOT NULL, bucket TEXT NOT NULL, action TEXT NOT NULL, "
    "x REAL, y REAL, pointer_id INTEGER, half_width_ns INTEGER, min_boottime_ns INTEGER, "
    "max_boottime_ns INTEGER, semantics TEXT, uncertainty_reason TEXT"
)
TAPS_COLUMNS_ADDED_BY_T6H = ("gesture_kind", "end_x", "end_y", "end_t_offset_ns", "path_json")
UPGRADE_JOB_ID = "gate-upgrade-failed-retryable-job"


def downgrade_timeline_to_5_columns(bundle_dir: Path, new_bundle_id: str) -> list[str]:
    """Rewrites `timeline.sqlite` into the pre-T-6H layout (taps without the 5 gesture columns),
    refreshes its manifest digest and gives the bundle a new id. Returns the remaining tap columns."""
    db = bundle_dir / "timeline.sqlite"
    con = sqlite3.connect(db)
    try:
        con.execute("PRAGMA journal_mode = DELETE")
        names = [row[1] for row in con.execute("PRAGMA table_info(taps)")]
        kept = [n for n in names if n not in TAPS_COLUMNS_ADDED_BY_T6H]
        con.executescript(
            f"BEGIN; CREATE TABLE taps_old ({TAPS_COLUMNS_BEFORE_T6H}); "
            f"INSERT INTO taps_old ({', '.join(kept)}) SELECT {', '.join(kept)} FROM taps; "
            "DROP TABLE taps; ALTER TABLE taps_old RENAME TO taps; "
            "CREATE INDEX idx_taps_bucket_boottime_ns ON taps(bucket, aligned_boottime_ns); COMMIT;"
        )
        con.execute("VACUUM")
        remaining = [row[1] for row in con.execute("PRAGMA table_info(taps)")]
    finally:
        con.close()
    for suffix in ("-wal", "-shm", "-journal"):
        (bundle_dir / f"timeline.sqlite{suffix}").unlink(missing_ok=True)
    manifest_path = bundle_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["bundle_id"] = new_bundle_id
    for entry in manifest["files"]:
        if entry["path"] == "timeline.sqlite":
            entry["sha256"] = sha256_file(db)
            entry["bytes"] = db.stat().st_size
    manifest_path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return remaining


def mutate_config_for_upgrade(config_path: Path, output_dir: Path, *, variant: str) -> dict[str, Any]:
    """Turns a freshly written `config.json` into what an older build left behind.

    variant "a": newer fields missing (crash cooldown, default save action, screenshot dir) AND unknown
    extra fields, with capture armed and a custom save window / output directory.
    variant "b": every known field present plus unknown extras at the top level."""
    cfg = json.loads(config_path.read_text(encoding="utf-8"))
    cfg["capture_enabled"] = True
    cfg["save_window_secs"] = 77
    cfg["output_dir_override"] = str(output_dir)
    if variant == "a":
        for key in ("crash_autosave_cooldown_secs", "ide_default_save_action", "screenshot_dir_override"):
            cfg.pop(key, None)
    cfg["future_experimental_flag"] = {"nested": [1, 2, 3], "enabled": True}
    cfg["future_string_field"] = "from-a-newer-build"
    config_path.write_text(json.dumps(cfg, indent=2), encoding="utf-8")
    return cfg


def seed_interrupted_pinned_job(catalog: Path, job_id: str, destination: Path) -> None:
    """Seed the old JSON chunk + pin journal + materialization parameters as a stopped daemon."""
    data = catalog.parent
    chunk_id = f"chunk-{job_id}"
    boot_id = f"boot-{job_id}"
    timestamp = "2026-11-24T00:00:00Z"
    ring = data / "ring"
    ring.mkdir(exist_ok=True)
    file_name = f"clock_metadata-{chunk_id}.chunk"
    heartbeat = {"boottime_ns": 100, "monotonic_ns": 100, "realtime_ns": 100, "streams": []}
    chunk = json.dumps({"events": [{"Heartbeat": {"pc_cont_ns": 100, "boot_id": boot_id, "payload": heartbeat}}]}).encode()
    (ring / file_name).write_bytes(chunk)
    (ring / f"{chunk_id}.meta.json").write_text(json.dumps({
        "chunk_id": chunk_id, "kind": "clock_metadata", "file_name": file_name,
        "created_cont_ns": 100, "duration_ns": 0, "bytes": len(chunk), "status": "closed",
        "sha256": hashlib.sha256(chunk).hexdigest(),
    }), encoding="utf-8")
    for name in ("save_jobs", "save_job_params"):
        (data / name).mkdir(exist_ok=True)
    (data / "save_jobs" / f"{job_id}.json").write_text(json.dumps({
        "job_id": job_id, "destination_path": str(destination),
        "pinned_chunk_ids": [chunk_id], "created_at_utc": timestamp,
    }), encoding="utf-8")
    (data / "save_job_params" / f"{job_id}.params.json").write_text(json.dumps({
        "bundle_id": f"bundle-{job_id}", "mode": "logs_only", "duration_sec": 30,
        "trigger_utc": timestamp, "tail_may_be_incomplete": True, "video_width": 0, "video_height": 0,
        "device": {"serial": "synthetic", "model": "upgrade fixture", "api_level": 33, "is_emulator": True},
        "window": {"kind": "real_elapsed", "boot_id": boot_id, "start_boottime_ns": 0,
                   "end_boottime_ns": 100, "ingest_start_cont_ns": 0, "ingest_end_cont_ns": 100},
        "policy": {"unanchored_ingest_margin_ns": 0, "max_unanchored_records": 64,
                   "max_unanchored_bytes": 8192, "drain_timeout_ms": 0},
    }), encoding="utf-8")
    con = sqlite3.connect(catalog)
    try:
        con.execute(
            "INSERT INTO save_jobs (job_id, state, destination_path, pinned_chunks_json, error_message, created_at_utc, updated_at_utc, trigger_kind) "
            "VALUES (?, 'pinned', ?, ?, NULL, '2026-11-24T00:00:00Z', '2026-11-24T00:00:00Z', 'manual')",
            (job_id, str(destination), json.dumps([chunk_id])),
        )
        con.commit()
    finally:
        con.close()


def catalog_rows(catalog: Path) -> dict[str, str]:
    con = sqlite3.connect(f"file:{catalog.as_posix()}?mode=ro", uri=True)
    try:
        return {row[0]: row[1] for row in con.execute("SELECT bundle_id, state FROM bundles")}
    finally:
        con.close()


def bundle_states(iso: Isolated) -> dict[str, str]:
    """bundle_id -> lifecycle state across active and trashed bundles, as the CLI reports them."""
    states: dict[str, str] = {}
    for state in ("active", "trashed"):
        res = iso.cli("bundles", "list", "--state", state)
        for item in (res.data or {}).get("bundles", []):
            states[item["bundle_id"]] = state
    return states


def run_new_launcher(ctx: dict[str, Any], iso: Isolated, *launcher_args: str) -> CliResult:
    root: Path = ctx["root"]
    if ctx["bundle_os"] == "windows":
        shell = find_powershell()
        if shell is None:
            return CliResult(127, "", "no PowerShell available to run start-arb.ps1", 0.0)
        cmd: list[Any] = [shell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", root / "start-arb.ps1", *launcher_args]
    else:
        cmd = ["bash", root / "start-arb.sh", *launcher_args]
    return run_proc(cmd, env=iso.env, timeout=180)


def status_hello(iso: Isolated) -> dict[str, Any]:
    return (iso.cli("status", timeout=30).data or {}).get("hello", {})


def section_upgrade(report: Report, ctx: dict[str, Any], fixtures: Path, args: argparse.Namespace) -> None:
    with report.section("upgrade", "G-M9: v2-old -> v2-new 稼働中 arbd のインプレースアップグレード") as sec:
        host_os = ctx["bundle_os"]
        new_root: Path = ctx["root"]
        work = ctx["work_dir"] / "upgrade"
        work.mkdir(parents=True, exist_ok=True)
        real_old = ctx.get("old_root") is not None
        old_root: Path = ctx["old_root"] if real_old else new_root
        sec.notes["mode"] = ("real previous release (identical reproducible arbd, distinct build_id forced)" if ctx.get("old_arbd_same_hash") else "real previous release") if real_old else "pseudo-old state on the same build"
        old = Isolated(old_root, work, host_os)
        new = Isolated(new_root, work, host_os, share_with=old)
        if real_old:
            old.extra_roots, new.extra_roots = [new_root], [old_root]
        tray: subprocess.Popen[bytes] | None = None
        try:
            old_sha = sha256_file(old.bin("arbd"))
            new_sha = sha256_file(new.bin("arbd"))
            replaced_expected = old_sha != new_sha
            if real_old:
                sec.check("U1: the old and new arbd are distinct builds (a real build_id hot-swap)", replaced_expected, f"old={old_sha[:12]} new={new_sha[:12]}")
                sec.notes["old_arbd_same_hash"] = bool(ctx.get("old_arbd_same_hash"))
            sec.check("old arbd cold-starts", old.start_arbd(), f"{old.bin('arbd')}")
            # ---- build the old state with the OLD binaries ---------------------------------
            managed = old.data / "bundles"
            managed.mkdir(parents=True, exist_ok=True)
            saved_a, _ = stage_fixture(fixtures, "gate-gop1s", work / "stage-a")
            saved_b, _ = stage_fixture(fixtures, "gate-gop5s", work / "stage-b")
            five_col, _ = stage_fixture(fixtures, "gate-gop1s", work / "stage-c")
            remaining = downgrade_timeline_to_5_columns(five_col, "legacy-5col-timeline")
            sec.check("fixture rewritten to the pre-T-6H 5-column timeline.sqlite layout", not set(remaining) & set(TAPS_COLUMNS_ADDED_BY_T6H) and "x" in remaining, remaining)
            ids = {}
            for label, src, dest_name in (("saved", saved_a, "saved-a"), ("trash-me", saved_b, "saved-b"), ("five-col", five_col, "five-col")):
                dest = managed / dest_name
                shutil.copytree(src, dest)
                opened = old.cli("bundles", "open", dest)
                ids[label] = (opened.data or {}).get("bundle_id")
                sec.check(f"old arbd registers the {label} bundle ({ids[label]})", opened.rc == 0 and bool(ids[label]), opened.err.strip()[:150])
                if (opened.data or {}).get("lease_id"):
                    old.cli("bundles", "close", opened.data["lease_id"])
            trimmed = old.cli("trim", managed / "saved-a", "--in-frame", "47", "--out-frame", "200", "--dest", managed)
            ids["trimmed"] = (trimmed.data or {}).get("bundle_id")
            sec.check("old arbd writes a trimmed bundle into the managed directory", trimmed.rc == 0 and bool(ids["trimmed"]), trimmed.err.strip()[:150])
            trashed = old.cli("bundles", "trash", ids["trash-me"] or "missing")
            sec.check("old arbd moves a bundle to the trash", trashed.rc == 0, trashed.err.strip()[:150])
            sec.check("old arbd stops cleanly", old.stop_arbd(), "")
            # ---- state only an older build could have left behind -------------------------------
            catalog = old.data / "catalog.sqlite"
            seed_interrupted_pinned_job(catalog, UPGRADE_JOB_ID, work / "retry-destination")
            retarget_id = f"{UPGRADE_JOB_ID}-retarget"
            seed_interrupted_pinned_job(catalog, retarget_id, work / "old-destination")
            config_path = old.data / "config.json"
            if not config_path.exists():
                old.start_arbd()
                old.cli("config", "set", "--save-window-secs", "300")
                old.stop_arbd()
            output_dir = work / "custom-output"
            output_dir.mkdir(exist_ok=True)
            cfg_old = mutate_config_for_upgrade(config_path, output_dir, variant="a")
            sec.check("config.json now looks like an older build's (fields missing, unknown extras, capture armed)", "ide_default_save_action" not in cfg_old and cfg_old["capture_enabled"] is True and "future_experimental_flag" in cfg_old)
            before_rows = catalog_rows(catalog)
            sec.check("catalog.sqlite holds saved, trimmed, trashed and five-column bundles", {"trashed"} <= set(before_rows.values()) and len(before_rows) >= 4, before_rows)
            sec.check("old arbd restarts on that state and serves it", old.start_arbd(), "")
            # Make the old writer attempt publication. A directory occupied by an unrelated
            # file fails on every host, including elevated Windows and Unix test runners.
            for job_id, dest in ((UPGRADE_JOB_ID, work / "retry-destination"),
                                 (retarget_id, work / "old-destination")):
                dest.mkdir()
                sentinel = dest / "keep.txt"
                sentinel.write_text("occupied", encoding="utf-8")
                failed = old.cli("jobs", "retry", job_id)
                job = (failed.data or {}).get("job", {})
                sec.check("old writer persists a failed job with its real ring pin",
                          job.get("state") == "failed_retryable"
                          and job.get("pinned_chunk_ids") == [f"chunk-{job_id}"]
                          and bool(job.get("last_error")) and sentinel.is_file(), job_id)
                if job_id == UPGRADE_JOB_ID:
                    sentinel.unlink()
                    dest.rmdir()
            old_hello = status_hello(old)
            old_pid = old_hello.get("pid")
            other = Isolated(new_root, work / "separate-instance", host_os)
            try:
                probe = run_new_launcher(ctx, new, "cli", "--json", "--runtime-dir", str(other.runtime),
                                         "--data-dir", str(other.data), "--cache-dir", str(other.cache), "status")
                sec.check("explicit CLI directories select a separate daemon", probe.rc == 0 and status_hello(other).get("pid") != old_pid)
                sec.check("launching in a separate runtime preserves the original daemon", status_hello(old).get("pid") == old_pid)
            finally:
                other.cleanup()
            before_states = bundle_states(old)
            sec.check("old arbd lists active and trashed bundles", "trashed" in before_states.values() and len(before_states) >= 4, before_states)
            sec.check("old arbd build_id is the old binary's", str(old_hello.get("build_id", "")).endswith(f":sha256:{old_sha}") or not old_hello.get("build_id"), old_hello.get("build_id"))
            # ---- U4: a tray of the new build is already running -----------------------------------
            if args.check_tray:
                if has_display():
                    tray = new.spawn("arb-tray", logname="upgrade-tray")
                    tray_stderr = new.last_logs[1]
                    time.sleep(4)
                    sec.check("U4: arb-tray (connected to the old arbd) is running before the upgrade", tray.poll() is None, f"returncode={tray.returncode}")
                else:
                    sec.check("U4: a GUI session / display is available for the tray", False, "use xvfb-run + dbus-run-session on headless Linux")
            # ---- the upgrade: the NEW launcher meets the running OLD arbd -------------------------
            launched = run_new_launcher(ctx, new, "cli", "--json", "bundles", "list")
            sec.check("new launcher runs `cli bundles list` (exit 0)", launched.rc == 0, f"rc={launched.rc} {launched.err.strip()[:300]}")
            if not new.wait_ready(30):
                sec.check("an arbd answers after the launcher ran", False, "")
                return
            hello = status_hello(new)
            build_id = str(hello.get("build_id", ""))
            sec.check("U3: running arbd's build_id is the NEW arbd binary's SHA-256", build_id.endswith(f":sha256:{new_sha}"), f"{build_id} vs {new_sha}")
            if replaced_expected:
                sec.check("U3: the old arbd process was replaced (pid changed)", hello.get("pid") != old_pid, f"{old_pid} -> {hello.get('pid')}")
                time.sleep(1)
                leftovers = list_processes_under(old_root)
                sec.check("U3: 0 old arbd processes remain", not [ln for ln in leftovers if "arbd" in ln and str(old_root) in ln], leftovers[:3])
                sec.check("U3: the launcher announced the replacement", "稼働中の arbd を更新します。" in launched.err, launched.err.strip()[:200])
            else:
                sec.check("same build: the running arbd was left untouched (non-destructive coexistence)", hello.get("pid") == old_pid, f"{old_pid} -> {hello.get('pid')}")
                sec.check("same build: the launcher did not announce a replacement", "稼働中の arbd を更新します。" not in launched.err, launched.err.strip()[:200])
            # ---- U3: state survives -----------------------------------------------------------
            cfg = (new.cli("config", "get").data or {}).get("config", {})
            sec.check("U3: config keeps save_window_secs=77, capture_enabled=true and the custom output dir", cfg.get("save_window_secs") == 77 and cfg.get("capture_enabled") is True and Path(str(cfg.get("output_dir_override", ""))) == output_dir, cfg)
            sec.check("U3: fields the old config lacked are filled with safe defaults", cfg.get("crash_autosave_cooldown_secs") == 15 and cfg.get("ide_default_save_action") == "save_and_open", {k: cfg.get(k) for k in ("crash_autosave_cooldown_secs", "ide_default_save_action", "screenshot_dir_override")})
            after_states = bundle_states(new)
            sec.check("U3: every catalog bundle and its state survived the upgrade", after_states == before_states, f"{before_states} -> {after_states}")
            sec.check("U3: catalog.sqlite rows are unchanged by the upgrade", catalog_rows(catalog) == before_rows, "")
            jobs = new.cli("jobs", "list")
            sec.check("U3: the retry-pending failed save job is still listed", UPGRADE_JOB_ID in jobs.out and "failed_retryable" in jobs.out, jobs.out.strip()[:200])
            restored = new.cli("bundles", "restore", ids["trash-me"] or "missing")
            sec.check("U3: bundle restore of the trashed bundle succeeds after the upgrade", restored.rc == 0, f"rc={restored.rc} {restored.err.strip()[:200]}")
            sec.check("U3: the restored bundle is active again", bundle_states(new).get(ids["trash-me"]) == "active", bundle_states(new))
            inspected = new.cli("inspect", ids["five-col"] or "missing")
            sec.check("U3: the pre-T-6H 5-column timeline.sqlite bundle opens (inspect)", inspected.rc == 0 and (inspected.data or {}).get("format") == "v2", f"rc={inspected.rc} {inspected.err.strip()[:200]}")
            client = McpClient([new.bin("arb-cli"), "mcp"], new.env)
            try:
                client.request("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "verify_release_bundle", "version": "1"}})
                overview = tool_text(client.call_tool("arb_timeline_overview", {"bundle_id": ids["five-col"]}))
                kinds = [t.get("gesture_kind") for t in overview.get("taps", [])]
                sec.check("U3: the 5-column bundle's taps read back (gesture_kind defaults to tap)", overview.get("frame_count") == 720 and len(kinds) == 4 and set(kinds) == {"tap"}, kinds)
            finally:
                client.close()
            trimmed_inspect = new.cli("inspect", ids["trimmed"] or "missing")
            sec.check("U3: the trimmed bundle is intact (provenance trimmed)", trimmed_inspect.rc == 0 and "trimmed" in trimmed_inspect.out, trimmed_inspect.err.strip()[:150])
            for job_id, action in ((UPGRADE_JOB_ID, ("retry", UPGRADE_JOB_ID)),
                                   (retarget_id, ("retarget", retarget_id, "--dest", str(work / "retargeted")))):
                journal = old.data / "save_jobs" / f"{job_id}.json"
                sec.check(f"U3: {action[0]} retains its pin until retry", journal.is_file())
                result = new.cli("jobs", *action)
                saved = Path((result.data or {}).get("bundle_path") or "")
                completed = result.rc == 0 and (saved / "timeline.sqlite").is_file()
                sec.check(f"U3: {action[0]} completes after upgrade", completed, result.err.strip()[:160])
                if completed:
                    with sqlite3.connect(f"file:{(saved / 'timeline.sqlite').as_posix()}?mode=ro", uri=True) as db:
                        anchors = db.execute("SELECT boot_id FROM clock_anchors").fetchall()
                    sec.check(f"U3: {action[0]} replayed the pinned chunk", (f"boot-{job_id}",) in anchors)
                    sec.check(f"U3: {action[0]} released its pin after commit", not journal.exists())
            # ---- config.json compatibility, variant b (unknown extras only) ---------------------------
            sec.check("new arbd stops cleanly before the config variant test", new.stop_arbd(), "")
            cfg_b = mutate_config_for_upgrade(config_path, output_dir, variant="b")
            sec.check("config variant b written (all known fields + unknown extras)", "future_string_field" in cfg_b)
            sec.check("new arbd starts on the variant-b config", new.start_arbd(), "")
            cfg_b_read = (new.cli("config", "get").data or {}).get("config", {})
            sec.check("config variant b is accepted and read back", cfg_b_read.get("save_window_secs") == 77 and cfg_b_read.get("capture_enabled") is True, cfg_b_read)
            # ---- U4: the tray survived the restart ----------------------------------------------
            if tray is not None:
                time.sleep(4)
                sec.check("U4: arb-tray is still alive after the arbd restarts", tray.poll() is None, f"returncode={tray.returncode}")
                second = new.spawn("arb-tray", logname="upgrade-tray2")
                second_err = new.last_logs[1]
                try:
                    second.wait(timeout=20)
                    err = second_err.read_text(encoding="utf-8", errors="replace")
                    sec.check("U4: the tray lock is intact (a second arb-tray exits at once, no duplicate)", second.returncode == 0 and "arb-tray.lock" in err, f"rc={second.returncode}")
                except subprocess.TimeoutExpired:
                    second.kill()
                    sec.check("U4: the tray lock is intact (a second arb-tray exits at once, no duplicate)", False, "second tray kept running")
                log = tray_stderr.read_text(encoding="utf-8", errors="replace")
                sec.check("U4: the tray noticed the arbd restart and reconnected to the new arbd", "arbd が応答を停止しました" in log and "arbd に再接続しました" in log, log.strip()[-300:])
                sec.check("U4: the tray logged no panic or crash", "panicked" not in log and "RUST_BACKTRACE" not in log, log.strip()[-200:])
                sec.check("U4: the tray exits on request after the upgrade", terminate_gracefully(tray), f"returncode={tray.returncode}")
        finally:
            old.cleanup()
            new.cleanup()
        leftovers = list_processes_under(new_root) + (list_processes_under(old_root) if real_old else [])
        sec.check("upgrade suite leaves 0 residual processes", not leftovers, leftovers[:5])


# --------------------------------------------------------------------------------------------
# self-test
# --------------------------------------------------------------------------------------------

def build_stub_elf(requirements: dict[str, Sequence[str]]) -> bytes:
    """A minimal ELF64 whose `.gnu.version_r` requires the given versioned symbols per library."""
    dynstr = bytearray(b"\0")

    def add(text: str) -> int:
        offset = len(dynstr)
        dynstr.extend(text.encode() + b"\0")
        return offset

    verneed = bytearray()
    for index, (library, names) in enumerate(requirements.items()):
        last = index == len(requirements) - 1
        verneed += struct.pack("<HHIII", 1, len(names), add(library), 16, 0 if last else 16 + 16 * len(names))
        for number, name in enumerate(names):
            verneed += struct.pack("<IHHII", 0, 0, 2 + number, add(name), 0 if number == len(names) - 1 else 16)
    shstrtab = b"\0.gnu.version_r\0.dynstr\0.shstrtab\0"
    body = bytes(verneed) + bytes(dynstr) + shstrtab
    shoff = (64 + len(body) + 7) & ~7
    headers = (
        struct.pack("<IIQQQQIIQQ", 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
        + struct.pack("<IIQQQQIIQQ", 1, SHT_GNU_VERNEED, 2, 0, 64, len(verneed), 2, len(requirements), 8, 0)
        + struct.pack("<IIQQQQIIQQ", 16, 3, 2, 0, 64 + len(verneed), len(dynstr), 0, 0, 1, 0)
        + struct.pack("<IIQQQQIIQQ", 24, 3, 0, 0, 64 + len(verneed) + len(dynstr), len(shstrtab), 0, 0, 1, 0)
    )
    header = b"\x7fELF" + bytes([2, 1, 1, 0]) + bytes(8) + struct.pack("<HHIQQQIHHHHHH", 2, 0x3E, 1, 0, 0, shoff, 0, 64, 0, 0, 64, 4, 3)
    return header + body + bytes(shoff - 64 - len(body)) + headers


def stub_binary(name: str, target_os: str) -> bytes:
    """Stand-in shipped binary; Linux ones carry a real glibc 2.34 / GLIBCXX 3.4.29 requirement table."""
    marker = f"stub-{name}-{target_os}".encode()
    if target_os != "linux":
        return marker
    return build_stub_elf({"libc.so.6": ["GLIBC_2.2.5", "GLIBC_2.34"], "libstdc++.so.6": ["GLIBCXX_3.4.29"]}) + marker


def build_stub_bundle(tmp: Path, target_os: str, arch: str, plugin_zip: Path | None) -> Path:
    packager = load_sibling("package_distribution")
    bin_dir = tmp / f"bin-{target_os}"
    bin_dir.mkdir(parents=True, exist_ok=True)
    for name in BINARIES:
        (bin_dir / exe(name, target_os)).write_bytes(stub_binary(name, target_os))
    agent = tmp / "agent"
    agent.mkdir(exist_ok=True)
    for jar in DEVICE_AGENT_JARS:
        (agent / jar).write_bytes(b"PK\x03\x04stub-" + jar.encode())
    return packager.build_distribution_zip(
        target_os=target_os,
        arch=arch,
        version="2.0.0-selftest",
        bin_dir=bin_dir,
        plugin_zip=plugin_zip,
        device_agent_dir=agent,
        permissions_doc_path=REPO_ROOT / "docs" / "testing" / "first-run-os-permissions.md",
        out_dir=tmp / f"dist-{target_os}",
        allow_missing_ide_plugin=plugin_zip is None,
        license_dir=REPO_ROOT,
    )


def build_stub_plugin(tmp: Path, slugs: Sequence[str], *, with_licenses: bool = True) -> Path:
    plugin = tmp / "arb-ide-plugin-2.0.0-selftest.zip"
    plugin_xml = (
        '<idea-plugin><description><![CDATA[Android Replay Buffer records the screen.]]></description>'
        '<change-notes><![CDATA[notes]]></change-notes>'
        '<idea-version since-build="262.9437.185" until-build="262.*"/>'
        '<depends optional="true" config-file="arb-android.xml">org.jetbrains.android</depends>'
        '<depends optional="true" config-file="arb-java.xml">com.intellij.modules.java</depends></idea-plugin>'
    )
    jar_buf = io.BytesIO()
    with zipfile.ZipFile(jar_buf, "w") as jar:
        jar.writestr("META-INF/plugin.xml", plugin_xml)
        jar.writestr("META-INF/arb-android.xml", "<idea-plugin/>")
        jar.writestr("META-INF/arb-java.xml", "<idea-plugin/>")
    with zipfile.ZipFile(plugin, "w") as zf:
        zf.writestr("arb-ide-plugin/lib/arb-ide-plugin-2.0.0-selftest.jar", jar_buf.getvalue())
        zf.writestr("arb-ide-plugin/lib/client-core.jar", b"stub")
        for jar in DEVICE_AGENT_JARS:
            zf.writestr(f"arb-ide-plugin/device-agent/{jar}", b"stub")
        if with_licenses:
            for lic in LICENSE_FILES:
                zf.writestr(f"arb-ide-plugin/{PLUGIN_LICENSE_DIR}/{lic}", (REPO_ROOT / lic).read_bytes())
        for slug in slugs:
            slug_os = slug.split("-", 1)[0]
            for name in BINARIES:
                info = zipfile.ZipInfo(f"arb-ide-plugin/native/{slug}/{exe(name, slug_os)}")
                info.external_attr = (stat.S_IFREG | 0o755) << 16
                zf.writestr(info, stub_binary(name, slug_os))
    return plugin


def run_static(tmp: Path, bundle_zip: Path, plugin_zip: Path | None, scope: str, arch: str) -> Report:
    work = Path(tempfile.mkdtemp(prefix=f"static-{bundle_zip.stem}-", dir=tmp))
    ctx = {"bundle_zip": bundle_zip, "plugin_zip": plugin_zip, "plugin_scope": scope, "bundle_arch": arch, "work_dir": work, "extract_dir": work / "extract"}
    report = Report({"platform": "selftest"}, quiet=True)
    section_bundle_integrity(report, ctx)
    return report


def self_test() -> int:
    failures: list[str] = []

    def require(name: str, cond: bool, detail: Any = "") -> None:
        if not cond:
            failures.append(f"{name}: {detail}")

    # A launcher can exit while its daemon still owns a copy of the output handles.
    launcher = "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time; time.sleep(5)'], stdout=sys.stdout, stderr=sys.stderr); print('ready')"
    launched = run_proc([sys.executable, "-c", launcher], timeout=2)
    require("launcher exits without waiting for daemon output handles", launched.rc == 0 and launched.out.strip() == "ready" and launched.secs < 4, launched.rc)

    # pure helpers
    require("normalize_os", normalize_os("Darwin") == "macos" and normalize_os("win32") == "windows" and normalize_os("Linux") == "linux")
    require("normalize_arch", normalize_arch("aarch64") == "arm64" and normalize_arch("AMD64") == "x86_64")
    require("exe suffix", exe("arbd", "windows") == "arbd.exe" and exe("arbd", "linux") == "arbd")
    require("version line", parse_version_line("arbd 2.0.0-dev\n", "arbd") == "2.0.0-dev" and parse_version_line("arbd2 1", "arbd") is None)
    require("retired label guard", retired_label_hits("ARB v2 beta") and not retired_label_hits("schema_version = 2 (MIT OR Apache-2.0)"))
    png = b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", 1020, 588) + b"\x08\x06\x00\x00\x00"
    require("png dims", png_dimensions(png) == (1020, 588) and png_dimensions(b"nope") is None)
    require("snapshot diff", diff_snapshots({"a": "1", "b": "2"}, {"a": "1", "b": "3", "c": "4"}) == ["modified: b", "added: c"])
    require("tool names", len(set(EXPECTED_MCP_TOOLS)) == 19)
    require("gesture probes", [p["label"] for p in gesture_probe_points({"gestures": [
        {"kind": "tap", "parent_frame": 20, "in_trim_range": False, "trim_frame": -27, "x": 1, "y": 1},
        {"kind": "tap", "parent_frame": 120, "in_trim_range": True, "trim_frame": 73, "x": 960.0, "y": 540.0},
        {"kind": "swipe", "parent_frame": 400, "in_trim_range": True, "trim_frame": 353, "x": 300.0, "y": 300.0, "end_x": 1500.0, "end_y": 300.0},
    ]})] == ["tap@120", "swipe-start@400", "swipe-mid@400"])

    # decoded-pixel comparison on synthetic RGB frames
    w, h = 200, 120
    base = bytes([100, 110, 120]) * (w * h)
    marked = bytearray(base)
    for y in range(40, 60):
        for x in range(90, 110):
            marked[(y * w + x) * 3:(y * w + x) * 3 + 3] = bytes([237, 195, 80])
    require("marker pixels counted", changed_pixels(base, bytes(marked), w, 100, 50, 30, h) == 400, changed_pixels(base, bytes(marked), w, 100, 50, 30, h))
    require("control window clean", changed_pixels(base, bytes(marked), w, 100 + 150, 50, 30, h) == 0 and changed_pixels(base, bytes(marked), w, 20, 20, 10, h) == 0)
    require("window clamps at borders", changed_pixels(base, bytes(marked), w, 0, 0, 30, h) == 0)

    # MCP client against a tiny fake server
    fake = (
        "import sys, json\n"
        "for line in sys.stdin:\n"
        "    m = json.loads(line)\n"
        "    if 'id' in m:\n"
        "        print(json.dumps({'jsonrpc': '2.0', 'id': m['id'], 'result': {'echo': m['method']}}), flush=True)\n"
    )
    client = McpClient([sys.executable, "-I", "-c", fake], dict(os.environ))
    try:
        require("mcp request/response", client.request("ping")["result"]["echo"] == "ping")
        client.notify("notifications/initialized")
        require("mcp notification is silent", client.request("tools/list")["result"]["echo"] == "tools/list" and not client.stray_frames)
    finally:
        stderr = client.close()
    require("mcp fake stderr empty", stderr == b"", stderr)

    with tempfile.TemporaryDirectory(prefix="arb-vrb-selftest-") as tmp_str:
        tmp = Path(tmp_str)
        # safe extraction rejects traversal
        evil = tmp / "evil.zip"
        with zipfile.ZipFile(evil, "w") as zf:
            zf.writestr("../escape.txt", b"x")
        try:
            safe_extract(evil, tmp / "evil-out")
            require("zip traversal rejected", False)
        except ValueError:
            pass

        # full static verification of stub bundles for the three OSes
        universal_plugin = build_stub_plugin(tmp, UNIVERSAL_NATIVE_SLUGS)
        for target_os, arch in (("linux", "x86_64"), ("macos", "arm64"), ("windows", "x86_64")):
            bundle = build_stub_bundle(tmp, target_os, arch, universal_plugin)
            rep = run_static(tmp, bundle, universal_plugin, "universal", arch)
            bad = [c for s in rep.sections for c in s.checks if not c["pass"]]
            require(f"static verification passes for {bundle.name}", rep.passed and not bad, bad[:3])
        incomplete = build_stub_bundle(tmp, "linux", "x86_64", None)
        rep = run_static(tmp, incomplete, universal_plugin, "universal", "x86_64")
        require("standalone plugin does not mask missing embedded ZIP", not rep.passed)
        # tampering is detected
        bundle = build_stub_bundle(tmp, "linux", "x86_64", universal_plugin)
        existing = tmp / "existing"
        existing.mkdir()
        marker = existing / "keep.txt"
        marker.touch()
        result = main(["--bundle-zip", str(bundle), "--static-only", "--work-dir", str(existing), "--out-dir", str(tmp / "report")])
        require("existing work directory is preserved", result == 2 and marker.exists())
        tampered = tmp / "tampered" / bundle.name
        tampered.parent.mkdir()
        with zipfile.ZipFile(bundle) as src, zipfile.ZipFile(tampered, "w") as dst:
            for info in src.infolist():
                data = src.read(info.filename)
                if info.filename.endswith("/LICENSE-MIT"):
                    data += b"tampered"
                dst.writestr(info, data)
        rep = run_static(tmp, tampered, universal_plugin, "universal", "x86_64")
        require("tampered LICENSE-MIT fails SHA256SUMS", not rep.passed)
        # missing license in the plugin and non-universal plugin are detected
        no_license_plugin = build_stub_plugin(tmp / "nl" if (tmp / "nl").mkdir() is None else tmp, UNIVERSAL_NATIVE_SLUGS, with_licenses=False)
        rep = run_static(tmp, bundle, no_license_plugin, "universal", "x86_64")
        require("plugin without licenses fails", not rep.passed)
        host_only = build_stub_plugin(tmp / "ho" if (tmp / "ho").mkdir() is None else tmp, ("linux-x86_64",))
        rep = run_static(tmp, build_stub_bundle(tmp, "linux", "x86_64", host_only), host_only, "universal", "x86_64")
        require("host-only plugin fails when universal is required", not rep.passed)
        rep = run_static(tmp, build_stub_bundle(tmp, "linux", "x86_64", host_only), host_only, "host-only", "x86_64")
        require("host-only plugin passes with --plugin-scope host-only", rep.passed, [c for s in rep.sections for c in s.checks if not c["pass"]][:3])
        # retired product-version label in a shipped file is detected
        label_repo = tmp / "label-repo"
        label_repo.mkdir()
        for lic in LICENSE_FILES:
            (label_repo / lic).write_bytes((REPO_ROOT / lic).read_bytes())
        (label_repo / "THIRD_PARTY_NOTICES.md").write_text((REPO_ROOT / "THIRD_PARTY_NOTICES.md").read_text(encoding="utf-8") + "\nARB " + "v" + "2 beta\n", encoding="utf-8")
        packager = load_sibling("package_distribution")
        stub_bin = tmp / "bin-linux"
        labelled = packager.build_distribution_zip(
            target_os="linux", arch="x86_64", version="2.0.0-selftest", bin_dir=stub_bin, plugin_zip=universal_plugin,
            device_agent_dir=tmp / "agent", permissions_doc_path=REPO_ROOT / "docs" / "testing" / "first-run-os-permissions.md",
            out_dir=tmp / "dist-label", license_dir=label_repo,
        )
        rep = run_static(tmp, labelled, universal_plugin, "universal", "x86_64")
        require("retired label in THIRD_PARTY_NOTICES.md is detected", not rep.passed)
        # ELF baseline guard: parser, pass at the limit, fail above it
        at_limit = build_stub_elf({"libc.so.6": ["GLIBC_2.2.5", "GLIBC_2.35"], "libstdc++.so.6": ["GLIBCXX_3.4.9", "GLIBCXX_3.4.30"], "libgcc_s.so.1": ["GCC_3.0"]})
        require("ELF verneed parser", elf_max_symbol_versions(at_limit) == {"GLIBC": (2, 35), "GLIBCXX": (3, 4, 30), "GCC": (3, 0)}, elf_max_symbol_versions(at_limit))
        grouped = bytearray(build_stub_elf({"libc.so.6": ["GLIBC_2.35"], "libstdc++.so.6": ["GLIBCXX_3.4.30"]}))
        grouped[80:96], grouped[96:112] = grouped[96:112], grouped[80:96]
        struct.pack_into("<II", grouped, 64 + 8, 32, 16)
        struct.pack_into("<I", grouped, 80 + 8, 32)
        require("ELF with grouped Verneed headers", elf_max_symbol_versions(grouped) ==
                {"GLIBC": (2, 35), "GLIBCXX": (3, 4, 30)})
        require("ELF at the baseline limit passes", elf_compat_problems(at_limit)[1] == [], elf_compat_problems(at_limit))
        too_new = build_stub_elf({"libc.so.6": ["GLIBC_2.34", "GLIBC_2.43", "GLIBC_PRIVATE"]})
        require("GLIBC_2.43 is rejected", elf_compat_problems(too_new)[1] == ["GLIBC_2.43 > GLIBC_2.35"], elf_compat_problems(too_new))
        require("GLIBC_2.36 is rejected and 2.9 is not newer than 2.35", elf_compat_problems(build_stub_elf({"libc.so.6": ["GLIBC_2.36"]}))[1] != [] and elf_compat_problems(build_stub_elf({"libc.so.6": ["GLIBC_2.9"]}))[1] == [])
        require("GLIBCXX_3.4.31 is rejected", elf_compat_problems(build_stub_elf({"libstdc++.so.6": ["GLIBCXX_3.4.31"]}))[1] == ["GLIBCXX_3.4.31 > GLIBCXX_3.4.30"])
        require("a binary without version needs passes", elf_compat_problems(build_stub_elf({}))[1] == [] and elf_max_symbol_versions(build_stub_elf({})) == {})
        invalid_link = bytearray(at_limit)
        shoff, = struct.unpack_from("<Q", invalid_link, 0x28)
        struct.pack_into("<I", invalid_link, shoff + 64 + 40, 99)
        empty_headers = bytearray(at_limit)
        struct.pack_into("<H", empty_headers, 0x3c, 0)
        for junk in (invalid_link, empty_headers, b"stub-arbd-linux", b"\x7fELF\x01\x01\x01" + bytes(80), at_limit[:100]):
            try:
                elf_max_symbol_versions(junk)
                require("non-ELF / truncated input raises", False)
            except ValueError:
                pass
        elf_dir = tmp / "elf-cli"
        elf_dir.mkdir()
        (elf_dir / "ok").write_bytes(at_limit)
        (elf_dir / "bad").write_bytes(too_new)
        require("--check-elf-compat exits 0 at the limit and 1 above it", check_elf_compat_cli([elf_dir / "ok"]) == 0 and check_elf_compat_cli([elf_dir / "ok", elf_dir / "bad"]) == 1)
        # a Linux bundle whose binary needs a newer glibc fails bundle-integrity
        new_glibc = tmp / "newglibc"
        new_glibc.mkdir()
        new_glibc_bundle = build_stub_bundle(new_glibc, "linux", "x86_64", universal_plugin)
        broken = new_glibc / "arb-linux-x86_64.zip"
        new_glibc_bundle.rename(new_glibc / "src.zip")
        with zipfile.ZipFile(new_glibc / "src.zip") as src, zipfile.ZipFile(broken, "w") as dst:
            for info in src.infolist():
                dst.writestr(info, too_new if info.filename.endswith("/bin/arbd") else src.read(info.filename))
        rep = run_static(tmp, broken, universal_plugin, "universal", "x86_64")
        glibc_checks = [c for s in rep.sections for c in s.checks if "Ubuntu 22.04" in c["name"]]
        require("bundle-integrity flags a binary that needs GLIBC_2.43", any(not c["pass"] and "GLIBC_2.43" in str(c["detail"]) for c in glibc_checks), glibc_checks)
        require("bundle-integrity passes the baseline for stub Linux bundles", all(c["pass"] for c in [c for s in run_static(tmp, bundle, universal_plugin, "universal", "x86_64").sections for c in s.checks if "Ubuntu 22.04" in c["name"]]))
        # previous release identical to the candidate: strict by default, opt-in keeps a real hot-swap
        probe = tmp / "arb-linux-x86_64.zip"
        for version, tag, matches in (
            ("2.0.0-dev.abcdef0", "early", True),
            ("2.0.0-dev.abcdef1", "early", False),
            ("2.0.0", "v2.0.0", True),
            ("2.0.1", "v2.0.0", False),
        ):
            with zipfile.ZipFile(probe, "w") as archive:
                archive.writestr(f"{probe.stem}/arb-ide-plugin-{version}.zip", b"")
            require(f"release provenance {version} / {tag}",
                    bundle_matches_release(probe, tag, "abcdef0" + "0" * 33) == matches)
        cand_zip = build_stub_bundle(tmp, "linux", "x86_64", universal_plugin)
        prev_dir = tmp / "prev-same"
        prev_dir.mkdir()
        prev_zip = prev_dir / cand_zip.name
        shutil.copyfile(cand_zip, prev_zip)

        def prepare(allow: bool) -> tuple[Report, dict[str, Any]]:
            work_dir = Path(tempfile.mkdtemp(prefix="prepare-old-", dir=tmp))
            pctx: dict[str, Any] = {"old_root": None, "bundle_zip": cand_zip, "plugin_zip": universal_plugin, "plugin_scope": "universal",
                                    "bundle_arch": "x86_64", "work_dir": work_dir, "extract_dir": work_dir / "extract"}
            prep = Report({"platform": "selftest"}, quiet=True)
            section_bundle_integrity(prep, pctx)
            ns = argparse.Namespace(upgrade_from_zip=prev_zip, allow_same_previous_arbd=allow)
            prepare_old_release(prep, pctx, ns)
            return prep, pctx

        prep, pctx = prepare(False)
        require("identical previous arbd is rejected by default", not prep.passed and not pctx.get("old_arbd_same_hash"),
                [c for s in prep.sections for c in s.checks if not c["pass"]][:2])
        prep, pctx = prepare(True)
        old_bin = pctx["old_root"] / "bin" / exe("arbd", "linux") if pctx["old_root"] else None
        new_bin = pctx["root"] / "bin" / exe("arbd", "linux")
        require("--allow-same-previous-arbd accepts an identical reproducible arbd", prep.passed and pctx.get("old_arbd_same_hash") is True,
                [c for s in prep.sections for c in s.checks if not c["pass"]][:2])
        require("--allow-same-previous-arbd forces a distinct previous arbd build", old_bin is not None and sha256_file(old_bin) != sha256_file(new_bin)
                and pctx["old_root"] != pctx["root"] and (pctx["old_root"] / "bin" / exe("arb-cli", "linux")).is_file())
        # a previous release that really differs needs no flag and stays untouched
        diff_dir = tmp / "prev-diff"
        diff_dir.mkdir()
        diff_zip = diff_dir / cand_zip.name
        with zipfile.ZipFile(cand_zip) as src, zipfile.ZipFile(diff_zip, "w") as dst:
            for info in src.infolist():
                data = src.read(info.filename)
                if info.filename.endswith("/bin/arbd"):
                    data += b"older"
                dst.writestr(info, data)
        prev_zip = diff_zip
        prep, pctx = prepare(False)
        require("a genuinely different previous arbd passes strictly", prep.passed and pctx["old_root"] is not None and not pctx.get("old_arbd_same_hash"),
                [c for s in prep.sections for c in s.checks if not c["pass"]][:2])
        # report rendering
        report = Report({"platform": "selftest", "bundle": "x", "core_version": "2.0.0-dev"}, quiet=True)
        with report.section("demo", "demo section") as sec:
            sec.check("ok", True, "d")
            sec.skip("os only", "reason")
        with report.section("crash", "crashing section"):
            raise RuntimeError("boom")
        require("crash is reported as a failed check", not report.passed and any("boom" in c["detail"] for c in report.sections[1].checks))
        md = report.to_markdown()
        require("markdown report", "demo" in md and "SKIP" in md and "FAIL" in md)
        json.dumps(report.to_json())

    if failures:
        print("SELF-TEST FAILED:")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("verify_release_bundle.py --self-test: OK (helpers, MCP framing, pixel comparison, static bundle/plugin verification for linux/macos/windows incl. tamper / missing-license / host-only / retired-label detection, ELF glibc / GLIBCXX baseline guard, previous-release identity strict / --allow-same-previous-arbd, report rendering)")
    return 0


# --------------------------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------------------------

def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--self-test", action="store_true", help="run the helper / static-verification self-test and exit")
    ap.add_argument("--same-core-previous-bundle", nargs=3, metavar=("ZIP", "TAG", "CANDIDATE"),
                    help="print whether a downloaded release bundle matches the tag and candidate core tree")
    ap.add_argument("--check-elf-compat", nargs="+", type=Path, metavar="BINARY", help="print the highest GLIBC / GLIBCXX each Linux binary requires and exit non-zero above the Ubuntu 22.04 baseline (glibc 2.35, GLIBCXX 3.4.30)")
    ap.add_argument("--bundle-zip", type=Path, help="arb-<os>-<arch>.zip to verify")
    ap.add_argument("--plugin-zip", type=Path, help="universal arb-ide-plugin-*.zip (also checked against the bundle-embedded copy)")
    ap.add_argument("--plugin-scope", choices=("universal", "host-only"), default="universal", help="require 3 OS native dirs (default) or only the bundle's OS")
    ap.add_argument("--out-dir", type=Path, default=Path("release-bundle-out"))
    ap.add_argument("--work-dir", type=Path, help="isolated work directory (default: a new temp dir outside the repository)")
    ap.add_argument("--keep-work", action="store_true", help="keep the work directory")
    ap.add_argument("--fixtures-dir", type=Path, help="directory holding gate-gop1s / gate-gop5s bundles produced by the arb_gate_bundle example")
    ap.add_argument("--generator", type=Path, help="path to the compiled arb_gate_bundle example (default: core/target/release/examples, else cargo run)")
    ap.add_argument("--legacy-fixtures", type=Path, default=REPO_ROOT / "fixtures" / "legacy-schema4", help="read-only schema 4 fixture tree")
    ap.add_argument("--upgrade-from-zip", type=Path, help="a previous arb-<os>-<arch>.zip: its arbd is started first and the new launcher must upgrade it in place (omit to self-verify with pseudo-old state)")
    ap.add_argument("--allow-same-previous-arbd", action="store_true", help="accept a --upgrade-from-zip whose arbd is byte-identical to the candidate (reproducible build, core/ unchanged since that release); the upgrade is then run against a copy of it with a distinct build_id. Default: an identical previous release fails")
    ap.add_argument("--check-tray", action="store_true", help="also verify arb-tray cold start, single instance and clean exit (needs a GUI session or Xvfb)")
    ap.add_argument("--static-only", action="store_true", help="only verify ZIP contents (use when the bundle OS differs from the host)")
    ap.add_argument("--expect-core-version", default=None, help="expected arbd/arb-cli/arb-tray --version (default: core/Cargo.toml when present)")
    return ap.parse_args(argv)


def prepare_old_release(report: Report, ctx: dict[str, Any], args: argparse.Namespace) -> None:
    """Extracts `--upgrade-from-zip` (a real previous release) next to the new bundle."""
    if not args.upgrade_from_zip:
        return
    with report.section("upgrade-source", "旧リリース ZIP の展開") as sec:
        zip_path = args.upgrade_from_zip.resolve()
        match = re.match(r"^(arb-(linux|macos|windows)-[a-z0-9_]+)\.zip$", zip_path.name)
        if not sec.check("previous release ZIP is named arb-<os>-<arch>.zip", zip_path.is_file() and bool(match), zip_path.name):
            return
        if not sec.check("previous release is for this OS", match.group(2) == ctx["bundle_os"], match.group(2)):  # type: ignore[union-attr]
            return
        old_extract = ctx["work_dir"] / "old-extract"
        safe_extract(zip_path, old_extract)
        old_root = old_extract / match.group(1)  # type: ignore[union-attr]
        ok = all((old_root / "bin" / exe(b, ctx["bundle_os"])).is_file() for b in BINARIES)
        sec.check("previous release carries arbd / arb-cli / arb-tray", ok, str(old_root))
        old_sha = sha256_file(old_root / "bin" / exe("arbd", ctx["bundle_os"])) if ok else ""
        new_sha = sha256_file(ctx["root"] / "bin" / exe("arbd", ctx["bundle_os"]))
        # A previous release that is byte-identical to the candidate proves nothing by itself, so it
        # fails by default. `--allow-same-previous-arbd` is for a reproducible build whose `core/`
        # did not change since the previous release: the same arbd is then expected, and the
        # upgrade is still exercised against a *distinct* build_id (see `distinct_old_release`).
        same = ok and old_sha == new_sha
        detail = f"old={old_sha[:12]} new={new_sha[:12]}"
        sec.notes["old_arbd_sha256"] = old_sha
        sec.notes["new_arbd_sha256"] = new_sha
        if same and args.allow_same_previous_arbd:
            sec.check("previous release arbd is the identical reproducible build (--allow-same-previous-arbd)", True,
                      f"identical reproducible arbd binary (old=new={old_sha[:12]}); U1–U4 will still force a distinct build_id hot-swap")
            old_root = distinct_old_release(sec, old_root, ctx["work_dir"], ctx["bundle_os"])
            if old_root is None:
                return
            ctx["old_arbd_same_hash"] = True
        else:
            sec.check("previous release arbd differs from the candidate (a real upgrade)", ok and old_sha != new_sha, detail)
        if ok:
            ctx["old_root"] = old_root


def distinct_old_release(sec: Section, old_root: Path, work_dir: Path, bundle_os: str) -> Path | None:
    """Copies the previous release and gives its `arbd` a different file hash.

    arbd's build_id is `<version>:sha256:<digest of its own executable>`, so the copy reports a
    different build_id from the candidate while running the identical code. The launcher then has
    a genuine old -> new build_id replacement to perform. Trailing bytes are ignored by ELF and PE
    loaders; a Mach-O needs its ad-hoc signature redone."""
    distinct = work_dir / "old-distinct" / old_root.name
    shutil.copytree(old_root, distinct)
    arbd = distinct / "bin" / exe("arbd", bundle_os)
    before = sha256_file(arbd)
    signed = True
    if bundle_os == "macos":
        # Appending to a Mach-O breaks its signature (strict validation fails), so keep the layout and
        # only re-sign ad hoc under another identifier: the code is unchanged, the file hash is not.
        resigned = run_proc(["codesign", "--force", "--sign", "-", "--identifier", "arb-gate-distinct-previous-build", arbd], timeout=60)
        verified = run_proc(["codesign", "--verify", "--strict", arbd], timeout=60) if resigned.rc == 0 else resigned
        signed = resigned.rc == 0 and verified.rc == 0
        sec.check("distinct previous arbd re-signed ad hoc and passes strict validation (macOS)", signed, (resigned.err + verified.err).strip()[:200])
    else:
        # ELF and PE loaders ignore trailing bytes.
        with arbd.open("ab") as fh:
            fh.write(b"\nARB-GATE-DISTINCT-PREVIOUS-BUILD\n")
    after = sha256_file(arbd)
    sec.notes["old_arbd_distinct_sha256"] = after
    sec.check("distinct previous arbd has a different SHA-256 from the candidate", signed and after != before, f"{before[:12]} -> {after[:12]}")
    return distinct if signed and after != before else None


def bundle_matches_release(bundle: Path, tag: str, commit: str) -> bool:
    # early uploads assets before moving its tag. Check the version inside the actual
    # OS archive so a concurrent refresh cannot supply a different previous build.
    prefix = f"{bundle.stem}/arb-ide-plugin-"
    with zipfile.ZipFile(bundle) as archive:
        versions = [name[len(prefix):-4] for name in archive.namelist()
                    if name.startswith(prefix) and name.endswith(".zip") and "/" not in name[len(prefix):]]
    if len(versions) != 1:
        return False
    if tag == "early":
        return versions[0].endswith(f"-dev.{commit[:7]}")
    return tag == f"v{versions[0]}"


def same_core_previous_bundle(bundle: Path, tag: str, candidate: str) -> bool:
    # Resolve the exact tag (including annotated tags) remotely. The checkout only
    # needs the candidate commit, so a depth-one checkout is sufficient.
    repo = os.environ["GH_REPO"]
    commit = json.loads(subprocess.check_output([
        "gh", "api", "-X", "GET", f"repos/{repo}/commits", "-f", f"sha=refs/tags/{tag}", "-F", "per_page=1",
    ]))[0]
    if not bundle_matches_release(bundle, tag, commit["sha"]):
        return False
    root = commit["commit"]["tree"]["sha"]
    tree = json.loads(subprocess.check_output(["gh", "api", f"repos/{repo}/git/trees/{root}"]))
    candidate_core = subprocess.check_output(["git", "rev-parse", f"{candidate}:core"], text=True).strip()
    return any(entry["path"] == "core" and entry["type"] == "tree" and entry["sha"] == candidate_core
               for entry in tree["tree"])


def check_elf_compat_cli(binaries: Sequence[Path]) -> int:
    failed = False
    for path in binaries:
        try:
            summary, problems = elf_compat_problems(path.read_bytes())
        except (OSError, ValueError) as err:
            summary, problems = str(err), [str(err)]
        failed = failed or bool(problems)
        print(f"{'FAIL' if problems else 'OK  '} {path.name}: {summary}{'; ' + ', '.join(problems) if problems else ''}")
    return 1 if failed else 0


def default_core_version() -> str | None:
    cargo = REPO_ROOT / "core" / "Cargo.toml"
    if not cargo.is_file():
        return None
    return load_sibling("package_distribution").read_workspace_version(cargo)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return self_test()
    if args.same_core_previous_bundle:
        bundle, tag, candidate = args.same_core_previous_bundle
        print("true" if same_core_previous_bundle(Path(bundle), tag, candidate) else "false")
        return 0
    if args.check_elf_compat:
        return check_elf_compat_cli(args.check_elf_compat)
    if args.bundle_zip is None:
        print("ERROR: --bundle-zip is required (or use --self-test)", file=sys.stderr)
        return 2
    if not args.bundle_zip.is_file():
        print(f"ERROR: bundle ZIP not found: {args.bundle_zip}", file=sys.stderr)
        return 2
    match = re.match(r"^arb-(linux|macos|windows)-([a-z0-9_]+)\.zip$", args.bundle_zip.name)
    if not match:
        print(f"ERROR: unexpected bundle name {args.bundle_zip.name}", file=sys.stderr)
        return 2
    bundle_os, bundle_arch = match.group(1), match.group(2)
    host_os = normalize_os()
    if bundle_os != host_os and not args.static_only:
        print(f"ERROR: bundle is for {bundle_os} but this host is {host_os}; pass --static-only to verify only the ZIP contents", file=sys.stderr)
        return 2
    out_dir = args.out_dir.resolve()
    if args.work_dir:
        work = args.work_dir.resolve()
        if (work == REPO_ROOT or REPO_ROOT in work.parents or out_dir == work or work in out_dir.parents):
            print("ERROR: use separate repository, work and report directories", file=sys.stderr)
            return 2
        try:
            work.mkdir(parents=True, exist_ok=False)
        except FileExistsError:
            print(f"ERROR: --work-dir must be a new directory: {work}", file=sys.stderr)
            return 2
    else:
        work = Path(tempfile.mkdtemp(prefix="arb-release-gate-")).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    host = {
        "platform": f"{platform.system()} {platform.release()} {platform.machine()}",
        "python": platform.python_version(),
        "bundle": args.bundle_zip.name,
        "bundle_sha256": sha256_file(args.bundle_zip),
        "plugin": args.plugin_zip.name if args.plugin_zip else None,
        "work_dir": str(work),
    }
    report = Report(host)
    ctx: dict[str, Any] = {
        "old_root": None,
        "old_arbd_same_hash": False,
        "bundle_zip": args.bundle_zip.resolve(),
        "plugin_zip": args.plugin_zip.resolve() if args.plugin_zip else None,
        "plugin_scope": args.plugin_scope,
        "bundle_arch": bundle_arch,
        "work_dir": work,
        "extract_dir": work / "extract",
        "out_dir": out_dir,
        "expect_core_version": args.expect_core_version or default_core_version(),
    }
    iso: Isolated | None = None
    try:
        section_bundle_integrity(report, ctx)
        if "root" not in ctx:
            return finish(report, out_dir, work, args.keep_work, ctx)
        if not args.static_only:
            iso = Isolated(ctx["root"], work, bundle_os)
            section_launchers(report, ctx, iso)
            section_parent_bound(report, iso)
            section_cold_start(report, ctx, iso)
            host["core_version"] = ctx.get("core_version")
            fixtures = None
            with report.section("fixtures", "決定論的フィクスチャ (Tap + Swipe + audio)") as fsec:
                fixtures = locate_or_generate_fixtures(args, work, fsec)
            if fixtures is not None and iso.arbd is not None and iso.arbd.poll() is None:
                staged, meta = stage_fixture(fixtures, "gate-gop1s", work / "mcp")
                section_mcp(report, ctx, iso, staged, meta)
                if args.legacy_fixtures.is_dir():
                    section_legacy(report, ctx, iso, args.legacy_fixtures.resolve())
                else:
                    with report.section("legacy-read-only", "旧形式 schema 3 / 4 の読取専用不変性") as lsec:
                        lsec.check("legacy fixture directory exists", False, str(args.legacy_fixtures))
                section_suite_b(report, ctx, iso, fixtures)
                section_burn_in(report, ctx, iso, fixtures)
            else:
                with report.section("mcp", "MCP / legacy / suite-b / burn-in") as sec:
                    sec.check("fixtures and a running arbd are available for the runtime suites", False, "see the fixtures / cold-start-ipc sections")
            if fixtures is not None:
                iso.stop_arbd()  # the upgrade suite starts its own daemons on separate directories
                prepare_old_release(report, ctx, args)
                section_upgrade(report, ctx, fixtures, args)
            if args.check_tray and iso is not None:
                section_tray(report, ctx, iso)
        else:
            section_launchers(report, ctx, None)
            host["core_version"] = None
    finally:
        if iso is not None:
            iso.cleanup()
    return finish(report, out_dir, work, args.keep_work, ctx)


def finish(report: Report, out_dir: Path, work: Path, keep_work: bool, ctx: dict[str, Any]) -> int:
    root = ctx.get("root")
    if root is not None and not keep_work:
        leftovers = list_processes_under(root)
        if leftovers:
            sec = Section("cleanup", "後始末")
            sec.check("no process from the extracted bundle survives the run", False, leftovers[:5])
            report.sections.append(sec)
    (out_dir / "release-bundle-report.json").write_text(json.dumps(report.to_json(), indent=2, ensure_ascii=False), encoding="utf-8")
    (out_dir / "release-bundle-report.md").write_text(report.to_markdown(), encoding="utf-8")
    print(report.to_markdown())
    if not keep_work:
        shutil.rmtree(work, ignore_errors=True)
    print(f"verify_release_bundle.py: {'PASS' if report.passed else 'FAIL'} -> {out_dir}")
    return 0 if report.passed else 1


if __name__ == "__main__":
    sys.exit(main())

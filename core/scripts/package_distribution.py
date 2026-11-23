#!/usr/bin/env python3
"""Cross-OS self-contained distribution packager for Android Replay Buffer (ARB) v2.

Pure Python 3 stdlib (Linux / macOS / Windows, UTF-8 safe).
Packages `arb-v2-<os>-<arch>.zip` containing:
  - `arb-ide-plugin-*.zip` (Android Studio plugin)
  - `bin/{arbd,arb-cli,arb-tray}` (`.exe` on Windows)
  - `device-agent/*.dex.jar` (`arb-companion.dex.jar`, `replay-scrcpy-audio.dex.jar`)
  - Platform launcher (`start-arb.sh` + `start-arb-tray.command` on macOS,
    `start-arb.sh` on Linux, `start-arb.ps1` on Windows)
  - `README-TRIAL.md` (with `docs/first-run-os-permissions.md` & MCP setup guide)
  - `SHA256SUMS.txt` (SHA-256 checksums for every packaged file)
"""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import stat
import sys
import tempfile
import zipfile
from pathlib import Path
from typing import Sequence

BINARIES = ("arbd", "arb-cli", "arb-tray")
DEVICE_AGENT_JARS = ("arb-companion.dex.jar", "replay-scrcpy-audio.dex.jar")
ZIP_TIMESTAMP = (2026, 11, 23, 0, 0, 0)


def normalize_os(raw: str | None) -> str:
    value = (raw or platform.system()).strip().lower()
    if value in ("darwin", "macos", "osx"):
        return "macos"
    if value in ("linux",):
        return "linux"
    if value in ("windows", "win32", "win"):
        return "windows"
    raise ValueError(f"unsupported target OS: {raw!r} (expected linux, macos, or windows)")


def normalize_arch(raw: str | None) -> str:
    value = (raw or platform.machine()).strip().lower()
    if value in ("arm64", "aarch64"):
        return "arm64"
    if value in ("x86_64", "amd64", "x64"):
        return "x86_64"
    if value:
        return value
    return "unknown"


def repo_root_from_script() -> Path:
    return Path(__file__).resolve().parent.parent.parent


def detect_version(repo_root: Path) -> str:
    cargo_toml = repo_root / "core" / "Cargo.toml"
    if cargo_toml.is_file():
        for line in cargo_toml.read_text(encoding="utf-8").splitlines():
            stripped = line.strip()
            if stripped.startswith("version") and "=" in stripped:
                _, rhs = stripped.split("=", 1)
                ver = rhs.strip().strip('"').strip("'")
                if ver:
                    return ver
    return "2.0.0-dev"


def binary_filename(name: str, target_os: str) -> str:
    return f"{name}.exe" if target_os == "windows" else name


def render_unix_launcher(target_os: str) -> str:
    os_note = "macOS" if target_os == "macos" else "Linux"
    return (
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        "\n"
        f"# Android Replay Buffer (ARB) v2 launcher ({os_note})\n"
        'ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"\n'
        'BIN_DIR="${ROOT_DIR}/bin"\n'
        'AGENT_DIR="${ROOT_DIR}/device-agent"\n'
        "\n"
        'if [[ -f "${AGENT_DIR}/arb-companion.dex.jar" ]]; then\n'
        '    export ARB_COMPANION_JAR="${AGENT_DIR}/arb-companion.dex.jar"\n'
        "fi\n"
        'if [[ -f "${AGENT_DIR}/replay-scrcpy-audio.dex.jar" ]]; then\n'
        '    export ARB_SCRCPY_JAR="${AGENT_DIR}/replay-scrcpy-audio.dex.jar"\n'
        "fi\n"
        'export ARB_DAEMON_PATH="${BIN_DIR}/arbd"\n'
        'export PATH="${BIN_DIR}:${PATH}"\n'
        "\n"
        "# If a stale arbd from a different/older package path is running (or lacks config.get),\n"
        "# stop it gracefully before starting this package's binaries.\n"
        "ensure_fresh_daemon() {\n"
        '    local expected_arbd="${BIN_DIR}/arbd"\n'
        '    local stale_pids=()\n'
        "    while IFS= read -r line; do\n"
        '        [[ -z "${line}" ]] && continue\n'
        '        local pid="${line%% *}"\n'
        '        local cmd="${line#* }"\n'
        '        local exe="${cmd%% *}"\n'
        '        local base="${exe##*/}"\n'
        '        if [[ "${base}" == "arbd" && "${exe}" != "${expected_arbd}" ]]; then\n'
        '            stale_pids+=("${pid}")\n'
        "        fi\n"
        '    done < <(ps -eo pid=,args= 2>/dev/null | sed -e "s/^ *//" || true)\n'
        "\n"
        "    local needs_stop=0\n"
        '    if (( ${#stale_pids[@]} > 0 )); then\n'
        "        needs_stop=1\n"
        '    elif "${BIN_DIR}/arb-cli" status >/dev/null 2>&1 && ! "${BIN_DIR}/arb-cli" config get >/dev/null 2>&1; then\n'
        "        needs_stop=1\n"
        "    fi\n"
        "\n"
        '    if [[ "${needs_stop}" -eq 1 ]]; then\n'
        '        echo "[arb] Stopping older arbd instance before launching ${expected_arbd}..." >&2\n'
        '        "${BIN_DIR}/arb-cli" daemon stop >/dev/null 2>&1 || true\n'
        "        for _ in 1 2 3 4 5 6 7 8 9 10; do\n"
        '            if ! "${BIN_DIR}/arb-cli" status >/dev/null 2>&1; then\n'
        "                break\n"
        "            fi\n"
        "            sleep 0.25\n"
        "        done\n"
        '        for pid in "${stale_pids[@]+"${stale_pids[@]}"}"; do\n'
        '            if kill -0 "${pid}" 2>/dev/null; then\n'
        '                kill "${pid}" 2>/dev/null || true\n'
        "            fi\n"
        "        done\n"
        "        sleep 0.2\n"
        "    fi\n"
        "}\n"
        "\n"
        'MODE="${1:-tray}"\n'
        "ensure_fresh_daemon\n"
        'case "${MODE}" in\n'
        "    tray)\n"
        '        exec "${BIN_DIR}/arb-tray" --auto-start\n'
        "        ;;\n"
        "    daemon)\n"
        '        exec "${BIN_DIR}/arbd"\n'
        "        ;;\n"
        "    mcp)\n"
        "        shift || true\n"
        '        exec "${BIN_DIR}/arb-cli" mcp "$@"\n'
        "        ;;\n"
        "    cli)\n"
        "        shift || true\n"
        '        exec "${BIN_DIR}/arb-cli" --auto-start "$@"\n'
        "        ;;\n"
        "    *)\n"
        '        echo "Usage: $0 [tray|daemon|mcp|cli ...]" >&2\n'
        "        exit 2\n"
        "        ;;\n"
        "esac\n"
    )


def render_macos_command_launcher() -> str:
    return (
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        "\n"
        "# Double-clickable Finder launcher for Android Replay Buffer (ARB) v2 menu bar tray\n"
        'ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"\n'
        'exec "${ROOT_DIR}/start-arb.sh" tray\n'
    )


def render_windows_launcher() -> str:
    return (
        "# Android Replay Buffer (ARB) v2 launcher (Windows PowerShell)\n"
        "param(\n"
        '    [ValidateSet("tray", "daemon", "mcp", "cli")]\n'
        '    [string]$Mode = "tray",\n'
        "    [Parameter(ValueFromRemainingArguments = $true)]\n"
        "    [string[]]$ExtraArgs\n"
        ")\n"
        "\n"
        '$ErrorActionPreference = "Stop"\n'
        "$RootDir = Split-Path -Parent $MyInvocation.MyCommand.Path\n"
        '$BinDir = Join-Path $RootDir "bin"\n'
        '$AgentDir = Join-Path $RootDir "device-agent"\n'
        '$ExpectedArbd = [System.IO.Path]::GetFullPath((Join-Path $BinDir "arbd.exe"))\n'
        '$CliExe = Join-Path $BinDir "arb-cli.exe"\n'
        "\n"
        '$CompanionJar = Join-Path $AgentDir "arb-companion.dex.jar"\n'
        "if (Test-Path $CompanionJar) { $env:ARB_COMPANION_JAR = $CompanionJar }\n"
        '$ScrcpyJar = Join-Path $AgentDir "replay-scrcpy-audio.dex.jar"\n'
        "if (Test-Path $ScrcpyJar) { $env:ARB_SCRCPY_JAR = $ScrcpyJar }\n"
        "$env:ARB_DAEMON_PATH = $ExpectedArbd\n"
        '$env:PATH = "$BinDir;$env:PATH"\n'
        "\n"
        "function Ensure-FreshDaemon {\n"
        '    $stale = @()\n'
        '    foreach ($proc in (Get-Process -Name "arbd" -ErrorAction SilentlyContinue)) {\n'
        "        $procPath = $null\n"
        "        try { $procPath = [System.IO.Path]::GetFullPath($proc.Path) } catch {}\n"
        "        if (-not $procPath -or -not $procPath.Equals($ExpectedArbd, [System.StringComparison]::OrdinalIgnoreCase)) {\n"
        "            $stale += $proc\n"
        "        }\n"
        "    }\n"
        "    $needsStop = ($stale.Count -gt 0)\n"
        "    if (-not $needsStop) {\n"
        "        & $CliExe status 2>$null | Out-Null\n"
        "        if ($LASTEXITCODE -eq 0) {\n"
        "            & $CliExe config get 2>$null | Out-Null\n"
        "            if ($LASTEXITCODE -ne 0) { $needsStop = $true }\n"
        "        }\n"
        "    }\n"
        "    if ($needsStop) {\n"
        '        [Console]::Error.WriteLine("[arb] Stopping older arbd instance before launching $ExpectedArbd...")\n'
        "        try { & $CliExe daemon stop 2>$null | Out-Null } catch {}\n"
        "        for ($i = 0; $i -lt 10; $i++) {\n"
        "            & $CliExe status 2>$null | Out-Null\n"
        "            if ($LASTEXITCODE -ne 0) { break }\n"
        "            Start-Sleep -Milliseconds 250\n"
        "        }\n"
        "        foreach ($proc in $stale) {\n"
        "            if (-not $proc.HasExited) {\n"
        "                Stop-Process -Id $proc.Id -ErrorAction SilentlyContinue\n"
        "            }\n"
        "        }\n"
        "        Start-Sleep -Milliseconds 200\n"
        "    }\n"
        "}\n"
        "\n"
        "Ensure-FreshDaemon\n"
        "\n"
        "switch ($Mode) {\n"
        '    "tray"   { & (Join-Path $BinDir "arb-tray.exe") --auto-start }\n'
        '    "daemon" { & $ExpectedArbd }\n'
        '    "mcp"    { & $CliExe mcp @ExtraArgs }\n'
        '    "cli"    { & $CliExe --auto-start @ExtraArgs }\n'
        "}\n"
    )


def render_readme_trial(
    *,
    target_os: str,
    arch: str,
    version: str,
    plugin_zip_name: str | None,
    has_device_agent: bool,
    permissions_doc: str,
) -> str:
    cli_bin = binary_filename("arb-cli", target_os)
    arbd_bin = binary_filename("arbd", target_os)
    if target_os == "windows":
        launcher_cmd = ".\\start-arb.ps1 tray"
        mcp_bin_example = f"C:\\\\path\\\\to\\\\arb-v2-{target_os}-{arch}\\\\bin\\\\{cli_bin}"
        arbd_path_example = f"C:\\\\path\\\\to\\\\arb-v2-{target_os}-{arch}\\\\bin\\\\{arbd_bin}"
    elif target_os == "macos":
        launcher_cmd = "./start-arb.sh tray  （または Finder で start-arb-tray.command をダブルクリック）"
        mcp_bin_example = f"/path/to/arb-v2-{target_os}-{arch}/bin/{cli_bin}"
        arbd_path_example = f"/path/to/arb-v2-{target_os}-{arch}/bin/{arbd_bin}"
    else:
        launcher_cmd = "./start-arb.sh tray"
        mcp_bin_example = f"/path/to/arb-v2-{target_os}-{arch}/bin/{cli_bin}"
        arbd_path_example = f"/path/to/arb-v2-{target_os}-{arch}/bin/{arbd_bin}"

    plugin_line = (
        f"- `{plugin_zip_name}`: Android Studio プラグイン（`Settings -> Plugins -> 歯車 -> Install Plugin from Disk...`）"
        if plugin_zip_name
        else "- `arb-ide-plugin-*.zip`: CI の `ide-plugin-tests` アーティファクトまたは `./gradlew buildPlugin` で生成"
    )
    agent_line = (
        "- `device-agent/*.dex.jar`: 端末常駐コンパニオンおよび音声取得用 DEX JAR（同梱済み）"
        if has_device_agent
        else "- `device-agent/*.dex.jar`: `./device-agent/build.sh --with-scrcpy-audio` で生成して配置"
    )

    return f"""# Android Replay Buffer (ARB) v2 トライアル配布パッケージ

- 対象プラットフォーム: `{target_os}-{arch}`
- バージョン: `{version}`
- ライセンス方針: MIT / Apache-2.0 / BSD-2-Clause / Unlicense のみ（**GPL / AGPL 依存 0 件**）

## 同梱構成

- `bin/{arbd_bin}`, `bin/{cli_bin}`, `bin/{binary_filename("arb-tray", target_os)}`: ローカルデーモン・CLI/MCP・メニューバー／システムトレイ
{plugin_line}
{agent_line}
- `docs/first-run-os-permissions.md`: macOS Gatekeeper / Windows SmartScreen 初回起動許可手順
- `SHA256SUMS.txt`: 同梱ファイルの SHA-256 チェックサム一覧

## 1. クイックスタート（トレイ＆デーモン）

1. 解凍したフォルダで次を実行します:
   ```text
   {launcher_cmd}
   ```
2. Android 端末（USB デバッグ有効）または Android Emulator を接続すると、常時リング録画と直前保存（既定 300s）・スクリーンショット・ログのみ保存が利用できます。

## 2. Android Studio プラグイン導入

1. Android Studio で **Settings → Plugins → 歯車アイコン → Install Plugin from Disk...** を開き、同梱のプラグイン ZIP を選択して再起動します。
2. **Settings → Tools → Android Replay Buffer** を開き、必要であれば **arbd バイナリパス上書き** に `{arbd_path_example}` を指定します。
   - 保持ウィンドウ（5〜600s）、保存先フォルダ、クラッシュ自動保存、既定保存アクション（保存して開く／保存のみ）を変更して Apply すると、IDE 再起動なしで即座にツールウィンドウおよび Running Devices / Logcat アクションへ反映されます。

## 3. MCP サーバー設定（AI エージェント連携）

`{cli_bin} mcp` は stdio JSON-RPC 2.0 準拠の MCP サーバーを起動します（`arb_timeline_overview`, `arb_inspect_frames`, `arb_export_trim`, `arb_save` など）。

Claude Code / Cursor / Codex の MCP 設定例:

```json
{{
  "mcpServers": {{
    "android-replay-buffer": {{
      "command": "{mcp_bin_example}",
      "args": ["mcp"]
    }}
  }}
}}
```

## 4. 初回起動の OS 許可（要約）

詳細は同梱の `docs/first-run-os-permissions.md` を参照してください。

---

{permissions_doc.strip()}
"""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def write_zip_entry(
    zf: zipfile.ZipFile,
    arcname: str,
    data: bytes,
    *,
    executable: bool = False,
) -> tuple[str, str]:
    info = zipfile.ZipInfo(filename=arcname, date_time=ZIP_TIMESTAMP)
    info.compress_type = zipfile.ZIP_DEFLATED
    mode = 0o755 if executable else 0o644
    info.external_attr = ((stat.S_IFREG | mode) & 0xFFFF) << 16
    zf.writestr(info, data)
    return arcname, sha256_bytes(data)


def resolve_plugin_zip(explicit: Path | None, repo_root: Path) -> Path | None:
    if explicit is not None:
        return explicit if explicit.is_file() else None
    dist_dir = repo_root / "ide-plugin" / "build" / "distributions"
    if not dist_dir.is_dir():
        return None
    candidates = sorted(dist_dir.glob("arb-ide-plugin-*.zip"))
    return candidates[-1] if candidates else None


def build_distribution_zip(
    *,
    target_os: str,
    arch: str,
    version: str,
    bin_dir: Path,
    plugin_zip: Path | None,
    device_agent_dir: Path | None,
    permissions_doc_path: Path,
    out_dir: Path,
    allow_missing_ide_plugin: bool = False,
    allow_missing_device_agent: bool = False,
) -> Path:
    target_os = normalize_os(target_os)
    arch = normalize_arch(arch)
    package_stem = f"arb-v2-{target_os}-{arch}"
    out_dir.mkdir(parents=True, exist_ok=True)
    zip_path = out_dir / f"{package_stem}.zip"

    # Validate required binaries upfront
    binary_paths: list[tuple[str, Path]] = []
    for name in BINARIES:
        fname = binary_filename(name, target_os)
        path = bin_dir / fname
        if not path.is_file():
            raise FileNotFoundError(f"missing required binary for {target_os}: {path}")
        binary_paths.append((fname, path))

    if plugin_zip is None and not allow_missing_ide_plugin:
        raise FileNotFoundError(
            "missing arb-ide-plugin-*.zip (build via `(cd ide-plugin && ./gradlew buildPlugin)` or pass --plugin-zip)"
        )

    agent_paths: list[tuple[str, Path]] = []
    if device_agent_dir is not None and device_agent_dir.is_dir():
        for jar_name in DEVICE_AGENT_JARS:
            candidate = device_agent_dir / jar_name
            if candidate.is_file():
                agent_paths.append((jar_name, candidate))
    if not agent_paths and not allow_missing_device_agent:
        raise FileNotFoundError(
            f"missing device-agent/*.dex.jar in {device_agent_dir} (build via `./device-agent/build.sh --with-scrcpy-audio`)"
        )

    if not permissions_doc_path.is_file():
        raise FileNotFoundError(f"missing permissions doc: {permissions_doc_path}")
    permissions_text = permissions_doc_path.read_text(encoding="utf-8")

    readme_text = render_readme_trial(
        target_os=target_os,
        arch=arch,
        version=version,
        plugin_zip_name=plugin_zip.name if plugin_zip is not None else None,
        has_device_agent=bool(agent_paths),
        permissions_doc=permissions_text,
    )

    checksums: list[tuple[str, str]] = []
    is_unix = target_os in ("linux", "macos")
    tmp_zip = out_dir / f".{package_stem}.zip.tmp.{os.getpid()}"

    try:
        with zipfile.ZipFile(tmp_zip, mode="w", compression=zipfile.ZIP_DEFLATED) as zf:
            for fname, path in binary_paths:
                checksums.append(
                    write_zip_entry(
                        zf,
                        f"{package_stem}/bin/{fname}",
                        path.read_bytes(),
                        executable=is_unix,
                    )
                )

            if plugin_zip is not None:
                checksums.append(
                    write_zip_entry(
                        zf,
                        f"{package_stem}/{plugin_zip.name}",
                        plugin_zip.read_bytes(),
                        executable=False,
                    )
                )

            for jar_name, jar_path in agent_paths:
                checksums.append(
                    write_zip_entry(
                        zf,
                        f"{package_stem}/device-agent/{jar_name}",
                        jar_path.read_bytes(),
                        executable=False,
                    )
                )

            if is_unix:
                checksums.append(
                    write_zip_entry(
                        zf,
                        f"{package_stem}/start-arb.sh",
                        render_unix_launcher(target_os).encode("utf-8"),
                        executable=True,
                    )
                )
                if target_os == "macos":
                    checksums.append(
                        write_zip_entry(
                            zf,
                            f"{package_stem}/start-arb-tray.command",
                            render_macos_command_launcher().encode("utf-8"),
                            executable=True,
                        )
                    )
            else:
                ps1_bytes = render_windows_launcher().replace("\n", "\r\n").encode("utf-8")
                checksums.append(
                    write_zip_entry(
                        zf,
                        f"{package_stem}/start-arb.ps1",
                        ps1_bytes,
                        executable=False,
                    )
                )

            checksums.append(
                write_zip_entry(
                    zf,
                    f"{package_stem}/README-TRIAL.md",
                    readme_text.encode("utf-8"),
                    executable=False,
                )
            )
            checksums.append(
                write_zip_entry(
                    zf,
                    f"{package_stem}/docs/first-run-os-permissions.md",
                    permissions_text.encode("utf-8"),
                    executable=False,
                )
            )

            sums_body = "".join(f"{digest}  {name}\n" for name, digest in sorted(checksums))
            write_zip_entry(
                zf,
                f"{package_stem}/SHA256SUMS.txt",
                sums_body.encode("utf-8"),
                executable=False,
            )

        os.replace(tmp_zip, zip_path)
    finally:
        if tmp_zip.exists():
            tmp_zip.unlink(missing_ok=True)

    return zip_path


def run_self_test(repo_root: Path) -> int:
    permissions_doc = repo_root / "docs" / "v2" / "testing" / "first-run-os-permissions.md"
    if not permissions_doc.is_file():
        print(f"ERROR: missing permissions doc for self-test: {permissions_doc}", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="arb-pkg-selftest-") as tmp_str:
        tmp = Path(tmp_str)
        out_dir = tmp / "dist"
        agent_dir = tmp / "device-agent"
        agent_dir.mkdir(parents=True)
        for jar in DEVICE_AGENT_JARS:
            (agent_dir / jar).write_bytes(b"PK\x03\x04stub-dex-jar-" + jar.encode("utf-8"))

        plugin_zip = tmp / "arb-ide-plugin-2.0.0-SNAPSHOT.zip"
        with zipfile.ZipFile(plugin_zip, "w") as pz:
            pz.writestr("arb-ide-plugin/lib/plugin.jar", b"stub-plugin-jar")

        matrix = [
            ("linux", "x86_64", ["start-arb.sh"]),
            ("macos", "arm64", ["start-arb.sh", "start-arb-tray.command"]),
            ("windows", "x86_64", ["start-arb.ps1"]),
        ]

        for target_os, arch, expected_launchers in matrix:
            bin_dir = tmp / f"bin-{target_os}"
            bin_dir.mkdir(parents=True)
            for name in BINARIES:
                (bin_dir / binary_filename(name, target_os)).write_bytes(
                    f"stub-{name}-{target_os}".encode("utf-8")
                )

            archive = build_distribution_zip(
                target_os=target_os,
                arch=arch,
                version="2.0.0-selftest",
                bin_dir=bin_dir,
                plugin_zip=plugin_zip,
                device_agent_dir=agent_dir,
                permissions_doc_path=permissions_doc,
                out_dir=out_dir,
            )
            stem = f"arb-v2-{target_os}-{arch}"
            assert archive.name == f"{stem}.zip", f"unexpected archive name: {archive.name}"

            with zipfile.ZipFile(archive, "r") as zf:
                names = set(zf.namelist())
                for name in BINARIES:
                    rel = f"{stem}/bin/{binary_filename(name, target_os)}"
                    assert rel in names, f"missing {rel} in {archive.name}"
                    if target_os != "windows":
                        mode = (zf.getinfo(rel).external_attr >> 16) & 0o777
                        assert mode == 0o755, f"expected 0755 on {rel}, got {oct(mode)}"

                assert f"{stem}/{plugin_zip.name}" in names, f"missing plugin zip in {archive.name}"
                for jar in DEVICE_AGENT_JARS:
                    assert f"{stem}/device-agent/{jar}" in names, f"missing {jar} in {archive.name}"

                for launcher in expected_launchers:
                    rel = f"{stem}/{launcher}"
                    assert rel in names, f"missing launcher {rel} in {archive.name}"
                    if target_os != "windows":
                        mode = (zf.getinfo(rel).external_attr >> 16) & 0o777
                        assert mode == 0o755, f"expected 0755 on {rel}, got {oct(mode)}"
                        if launcher == "start-arb.sh":
                            raw_sh = zf.read(rel).decode("utf-8")
                            assert "ensure_fresh_daemon" in raw_sh
                            assert "daemon stop" in raw_sh and "config get" in raw_sh
                    else:
                        raw_ps1 = zf.read(rel).decode("utf-8")
                        assert "\r\n" in raw_ps1 and "arbd.exe" in raw_ps1
                        assert "Ensure-FreshDaemon" in raw_ps1
                        assert "daemon stop" in raw_ps1 and "config get" in raw_ps1

                readme = zf.read(f"{stem}/README-TRIAL.md").decode("utf-8")
                assert "MCP" in readme and "first-run-os-permissions.md" in readme
                assert "GPL / AGPL" in readme
                perm = zf.read(f"{stem}/docs/first-run-os-permissions.md").decode("utf-8")
                assert "Gatekeeper" in perm and "SmartScreen" in perm

                sums = zf.read(f"{stem}/SHA256SUMS.txt").decode("utf-8").splitlines()
                assert len(sums) >= 8, f"expected >= 8 checksum lines, got {len(sums)}"
                for line in sums:
                    digest, rel = line.split("  ", 1)
                    assert sha256_bytes(zf.read(rel)) == digest, f"checksum mismatch for {rel}"

        print(
            "package_distribution.py --self-test: OK "
            "(verified linux-x86_64, macos-arm64, windows-x86_64 archives, POSIX modes, UTF-8 docs, SHA256SUMS)"
        )
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Package self-contained 3-OS distribution ZIP for Android Replay Buffer v2."
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run cross-OS packaging self-test across linux, macos, and windows fixtures.",
    )
    parser.add_argument("--target-os", default=None, help="Target OS (linux, macos, windows).")
    parser.add_argument("--arch", default=None, help="Target architecture (arm64, x86_64).")
    parser.add_argument("--version", default=None, help="Version label embedded in README-TRIAL.md.")
    parser.add_argument("--bin-dir", type=Path, default=None, help="Directory with arbd/arb-cli/arb-tray.")
    parser.add_argument("--plugin-zip", type=Path, default=None, help="Path to arb-ide-plugin-*.zip.")
    parser.add_argument("--device-agent-dir", type=Path, default=None, help="Directory with *.dex.jar.")
    parser.add_argument("--out-dir", type=Path, default=None, help="Output directory for distribution ZIP.")
    parser.add_argument(
        "--allow-missing-ide-plugin",
        action="store_true",
        help="Allow packaging without arb-ide-plugin-*.zip (e.g., Rust-only matrix CI jobs).",
    )
    parser.add_argument(
        "--allow-missing-device-agent",
        action="store_true",
        help="Allow packaging without device-agent/*.dex.jar (e.g., jobs without Android SDK).",
    )
    args = parser.parse_args(argv)

    repo_root = repo_root_from_script()
    if args.self_test:
        return run_self_test(repo_root)

    target_os = normalize_os(args.target_os)
    arch = normalize_arch(args.arch)
    version = args.version or detect_version(repo_root)
    bin_dir = args.bin_dir or (repo_root / "core" / "target" / "release")
    plugin_zip = resolve_plugin_zip(args.plugin_zip, repo_root)
    device_agent_dir = args.device_agent_dir or (repo_root / "device-agent" / "build")
    out_dir = args.out_dir or (repo_root / "dist")
    permissions_doc = repo_root / "docs" / "v2" / "testing" / "first-run-os-permissions.md"

    archive = build_distribution_zip(
        target_os=target_os,
        arch=arch,
        version=version,
        bin_dir=bin_dir,
        plugin_zip=plugin_zip,
        device_agent_dir=device_agent_dir,
        permissions_doc_path=permissions_doc,
        out_dir=out_dir,
        allow_missing_ide_plugin=args.allow_missing_ide_plugin,
        allow_missing_device_agent=args.allow_missing_device_agent,
    )
    print(f"Packaged {archive} ({archive.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

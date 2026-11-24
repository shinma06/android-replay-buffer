#!/usr/bin/env python3
"""Cross-OS self-contained distribution packager for Android Replay Buffer (ARB).

Pure Python 3 stdlib (Linux / macOS / Windows, UTF-8 safe).
Packages `arb-<os>-<arch>.zip` containing:
  - `arb-ide-plugin-*.zip` (Android Studio plugin)
  - `bin/{arbd,arb-cli,arb-tray}` (`.exe` on Windows)
  - `device-agent/*.dex.jar` (`arb-companion.dex.jar`, `replay-scrcpy-audio.dex.jar`)
  - Platform launcher (`start-arb.sh` + `start-arb-tray.command` on macOS,
    `start-arb.sh` on Linux, `start-arb.ps1` on Windows)
  - `README-TRIAL.md` (with `docs/first-run-os-permissions.md` & MCP setup guide)
  - `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD_PARTY_NOTICES.md` (dual license + third-party attribution;
    also placed under `licenses/` of the universal IDE plugin ZIP)
  - `SHA256SUMS.txt` (SHA-256 checksums for every packaged file)

Also classifies release tags for `.github/workflows/release.yml` (`--classify-tag`): only
`vX.Y.Z` is a stable `--latest` release; `vX.Y.Z-{alpha,beta,rc}.N` is published with
`--prerelease --latest=false`; anything else is rejected.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import re
import stat
import sys
import tempfile
import zipfile
from pathlib import Path
from typing import Sequence

BINARIES = ("arbd", "arb-cli", "arb-tray")
DEVICE_AGENT_JARS = ("arb-companion.dex.jar", "replay-scrcpy-audio.dex.jar")
# Repository-root legal files shipped in every bundle and in the IDE plugin (`<plugin>/licenses/`).
LICENSE_FILES = ("LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_NOTICES.md")
PLUGIN_LICENSE_DIR = "licenses"
ZIP_TIMESTAMP = (2026, 11, 23, 0, 0, 0)
REQUIRED_RELEASE_OSES = ("linux", "macos", "windows")
BUNDLE_ZIP_RE = re.compile(r"^arb-(linux|macos|windows)-([a-z0-9_]+)\.zip$")
_SEMVER_CORE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
STABLE_TAG_RE = re.compile(rf"^v{_SEMVER_CORE}$")
PRERELEASE_TAG_RE = re.compile(rf"^v{_SEMVER_CORE}-(alpha|beta|rc)\.(0|[1-9][0-9]*)$")


def classify_release_tag(tag: str) -> dict[str, object]:
    """Classifies a release tag (M12).

    `vX.Y.Z` -> stable (`gh release create --latest`); `vX.Y.Z-alpha.N` / `-beta.N` / `-rc.N` ->
    prerelease (`--prerelease --latest=false`, never marked as the latest stable release).
    Raises `ValueError` for any other tag so the release workflow fails before publishing.
    """
    value = tag.strip()
    if STABLE_TAG_RE.fullmatch(value):
        return {
            "tag": value,
            "version": value[1:],
            "channel": "stable",
            "prerelease": False,
            "gh_release_flags": ["--latest"],
        }
    if PRERELEASE_TAG_RE.fullmatch(value):
        return {
            "tag": value,
            "version": value[1:],
            "channel": "prerelease",
            "prerelease": True,
            "gh_release_flags": ["--prerelease", "--latest=false"],
        }
    raise ValueError(
        f"unsupported release tag {tag!r}: expected vX.Y.Z (stable) or "
        "vX.Y.Z-alpha.N / -beta.N / -rc.N (prerelease)"
    )


def write_tag_classification(
    classification: dict[str, object],
    github_output: Path | None,
    *,
    echo: bool = True,
) -> None:
    flags = " ".join(str(flag) for flag in classification["gh_release_flags"])  # type: ignore[union-attr]
    lines = [
        f"version={classification['version']}",
        f"channel={classification['channel']}",
        f"prerelease={'true' if classification['prerelease'] else 'false'}",
        f"gh_release_flags={flags}",
    ]
    if github_output is not None:
        with github_output.open("a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    if echo:
        print("\n".join(lines))


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


def read_workspace_version(cargo_toml: Path) -> str | None:
    """Returns `[workspace.package] version` of a Cargo manifest (None when absent)."""
    if not cargo_toml.is_file():
        return None
    in_section = False
    for line in cargo_toml.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_section = stripped == "[workspace.package]"
            continue
        if in_section and stripped.startswith("version") and "=" in stripped:
            _, rhs = stripped.split("=", 1)
            ver = rhs.split("#", 1)[0].strip().strip('"').strip("'")
            if ver:
                return ver
    return None


def detect_version(repo_root: Path) -> str:
    return read_workspace_version(repo_root / "core" / "Cargo.toml") or "2.0.0-dev"


def check_core_version(repo_root: Path, expected: str) -> str | None:
    """Returns an error message unless `core/Cargo.toml` carries exactly `expected`, so a tagged
    release can never ship binaries whose `--version` disagrees with the tag."""
    actual = read_workspace_version(repo_root / "core" / "Cargo.toml")
    if actual != expected:
        return (
            f"core/Cargo.toml [workspace.package] version is {actual!r} but the release is {expected!r}; "
            "bump core/Cargo.toml (and core/Cargo.lock) before tagging"
        )
    return None


def read_license_files(license_dir: Path) -> list[tuple[str, bytes]]:
    """Reads LICENSE-MIT / LICENSE-APACHE / THIRD_PARTY_NOTICES.md from `license_dir` (repo root)."""
    out: list[tuple[str, bytes]] = []
    for name in LICENSE_FILES:
        path = license_dir / name
        if not path.is_file():
            raise FileNotFoundError(f"missing legal file for distribution: {path}")
        out.append((name, path.read_bytes()))
    return out


def binary_filename(name: str, target_os: str) -> str:
    return f"{name}.exe" if target_os == "windows" else name


def render_unix_launcher(target_os: str) -> str:
    os_note = "macOS" if target_os == "macos" else "Linux"
    return (
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        "\n"
        f"# Android Replay Buffer (ARB) launcher ({os_note})\n"
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
        'export ARB_DAEMON_BIN="${BIN_DIR}/arbd"\n'
        'MODE="${1:-tray}"\n'
        'case "${MODE}" in\n'
        '    tray) "${BIN_DIR}/arb-cli" daemon prepare-upgrade --start ;;\n'
        '    daemon) "${BIN_DIR}/arb-cli" daemon prepare-upgrade ;;\n'
        'esac\n'
        'case "${MODE}" in\n'
        "    tray)\n"
        '        exec "${BIN_DIR}/arb-tray" --auto-start\n'
        "        ;;\n"
        "    daemon)\n"
        '        exec "${BIN_DIR}/arbd"\n'
        "        ;;\n"
        "    mcp)\n"
        "        shift || true\n"
        '        exec "${BIN_DIR}/arb-cli" --upgrade-if-needed mcp "$@"\n'
        "        ;;\n"
        "    cli)\n"
        "        shift || true\n"
        '        exec "${BIN_DIR}/arb-cli" --upgrade-if-needed --auto-start "$@"\n'
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
        "# Double-clickable Finder launcher for Android Replay Buffer (ARB) menu bar tray\n"
        'ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"\n'
        'exec "${ROOT_DIR}/start-arb.sh" tray\n'
    )


def render_windows_launcher() -> str:
    return (
        "# Android Replay Buffer (ARB) launcher (Windows PowerShell)\n"
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
        "$env:ARB_DAEMON_BIN = $ExpectedArbd\n"
        'if ($Mode -eq "daemon") { & $CliExe daemon prepare-upgrade }\n'
        'elseif ($Mode -eq "tray") { & $CliExe daemon prepare-upgrade --start }\n'
        'if (($Mode -in "tray", "daemon") -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n'
        "switch ($Mode) {\n"
        '    "tray"   { & (Join-Path $BinDir "arb-tray.exe") --auto-start }\n'
        '    "daemon" { & $ExpectedArbd }\n'
        '    "mcp"    { & $CliExe --upgrade-if-needed mcp @ExtraArgs }\n'
        '    "cli"    { & $CliExe --upgrade-if-needed --auto-start @ExtraArgs }\n'
        "}\n"
        "exit $LASTEXITCODE\n"
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
        mcp_bin_example = f"C:\\\\path\\\\to\\\\arb-{target_os}-{arch}\\\\bin\\\\{cli_bin}"
        arbd_path_example = f"C:\\\\path\\\\to\\\\arb-{target_os}-{arch}\\\\bin\\\\{arbd_bin}"
    elif target_os == "macos":
        launcher_cmd = "./start-arb.sh tray  （または Finder で start-arb-tray.command をダブルクリック）"
        mcp_bin_example = f"/path/to/arb-{target_os}-{arch}/bin/{cli_bin}"
        arbd_path_example = f"/path/to/arb-{target_os}-{arch}/bin/{arbd_bin}"
    else:
        launcher_cmd = "./start-arb.sh tray"
        mcp_bin_example = f"/path/to/arb-{target_os}-{arch}/bin/{cli_bin}"
        arbd_path_example = f"/path/to/arb-{target_os}-{arch}/bin/{arbd_bin}"

    plugin_line = (
        f"- `{plugin_zip_name}`: Android Studio プラグイン（`Settings -> Plugins -> 歯車 -> Install Plugin from Disk...`）"
        if plugin_zip_name
        else "- `arb-ide-plugin-*.zip`: GitHub Releases / CI の `test` アーティファクト、または `(cd ide-plugin && ./gradlew buildPlugin)` で生成"
    )
    agent_line = (
        "- `device-agent/*.dex.jar`: 端末常駐コンパニオンおよび音声取得用 DEX JAR（同梱済み）"
        if has_device_agent
        else "- `device-agent/*.dex.jar`: `./device-agent/build.sh --with-scrcpy-audio` で生成して配置"
    )

    return f"""# Android Replay Buffer (ARB) トライアル配布パッケージ

- 対象プラットフォーム: `{target_os}-{arch}`
- バージョン: `{version}`
- ライセンス: `MIT OR Apache-2.0`（`LICENSE-MIT` / `LICENSE-APACHE`）。第三者ソフトウェアは `THIRD_PARTY_NOTICES.md` を参照
- 依存ライセンス方針: 許容ライセンスは THIRD_PARTY_NOTICES.md を参照（**GPL / AGPL 依存 0 件**）

## 同梱構成

- `bin/{arbd_bin}`, `bin/{cli_bin}`, `bin/{binary_filename("arb-tray", target_os)}`: ローカルデーモン・CLI/MCP・メニューバー／システムトレイ
{plugin_line}
{agent_line}
- `docs/first-run-os-permissions.md`: macOS Gatekeeper / Windows SmartScreen 初回起動許可手順
- `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD_PARTY_NOTICES.md`: ライセンス全文と第三者ソフトウェアの帰属表示
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
    license_dir: Path | None = None,
) -> Path:
    target_os = normalize_os(target_os)
    arch = normalize_arch(arch)
    package_stem = f"arb-{target_os}-{arch}"
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
    license_blobs = read_license_files(license_dir or repo_root_from_script())

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
            for license_name, license_data in license_blobs:
                checksums.append(
                    write_zip_entry(
                        zf,
                        f"{package_stem}/{license_name}",
                        license_data,
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


def assemble_universal_release(
    *,
    input_dir: Path,
    out_dir: Path,
    version: str,
    permissions_doc_path: Path,
    license_dir: Path | None = None,
) -> list[Path]:
    """Assemble a universal 3-OS zero-zsh plugin ZIP and symmetric 3-OS bundles.

    Reads per-OS `arb-<os>-<arch>.zip` bundles and a base `arb-ide-plugin-*.zip`
    found recursively under `input_dir`, merges all 3 OS native binaries + both
    `device-agent/*.dex.jar` files into one universal `arb-ide-plugin-<version>.zip`,
    and rebuilds every `arb-<os>-<arch>.zip` with `device-agent/*.dex.jar`,
    the universal plugin ZIP, and fresh `SHA256SUMS.txt`.
    """
    if not input_dir.is_dir():
        raise FileNotFoundError(f"missing release input directory: {input_dir}")
    license_blobs = read_license_files(license_dir or repo_root_from_script())

    base_plugins: list[Path] = []
    os_bundles: dict[tuple[str, str], Path] = {}
    for candidate in sorted(input_dir.rglob("*.zip")):
        name = candidate.name
        if name.startswith("arb-ide-plugin-"):
            base_plugins.append(candidate)
            continue
        match = BUNDLE_ZIP_RE.match(name)
        if match:
            key = (match.group(1), match.group(2))
            if key in os_bundles:
                raise ValueError(f"duplicate OS bundle for {key[0]}-{key[1]}: {os_bundles[key]} and {candidate}")
            os_bundles[key] = candidate

    if len(base_plugins) != 1:
        raise ValueError(f"expected exactly 1 base arb-ide-plugin-*.zip in {input_dir}, found {base_plugins}")
    base_plugin_zip = base_plugins[0]

    present_oses = {os_name for os_name, _ in os_bundles}
    missing_oses = [o for o in REQUIRED_RELEASE_OSES if o not in present_oses]
    if missing_oses:
        raise FileNotFoundError(
            f"missing required OS bundle(s) in {input_dir}: {', '.join(missing_oses)} "
            f"(found: {', '.join(f'{o}-{a}' for o, a in sorted(os_bundles)) or 'none'})"
        )

    out_dir.mkdir(parents=True, exist_ok=True)
    universal_plugin_zip = out_dir / f"arb-ide-plugin-{version}.zip"

    with tempfile.TemporaryDirectory(prefix="arb-assemble-release-") as tmp_str:
        tmp = Path(tmp_str)
        extracted_bins: dict[tuple[str, str], Path] = {}
        agent_dir = tmp / "device-agent"
        agent_dir.mkdir(parents=True)

        # Extract native binaries and any device-agent DEX jars from per-OS bundles.
        for (target_os, arch), bundle_path in sorted(os_bundles.items()):
            stem = f"arb-{target_os}-{arch}"
            bin_dir = tmp / "bins" / f"{target_os}-{arch}"
            bin_dir.mkdir(parents=True)
            with zipfile.ZipFile(bundle_path, "r") as zf:
                names = set(zf.namelist())
                for bin_name in BINARIES:
                    fname = binary_filename(bin_name, target_os)
                    entry = f"{stem}/bin/{fname}"
                    if entry not in names:
                        raise FileNotFoundError(f"missing {entry} in {bundle_path}")
                    (bin_dir / fname).write_bytes(zf.read(entry))
                for jar_name in DEVICE_AGENT_JARS:
                    entry = f"{stem}/device-agent/{jar_name}"
                    if entry in names and not (agent_dir / jar_name).is_file():
                        (agent_dir / jar_name).write_bytes(zf.read(entry))
            extracted_bins[(target_os, arch)] = bin_dir

        # Fallback: also check base plugin ZIP for device-agent DEX jars.
        with zipfile.ZipFile(base_plugin_zip, "r") as bzf:
            base_names = bzf.namelist()
            if not base_names:
                raise ValueError(f"base plugin ZIP is empty: {base_plugin_zip}")
            plugin_root = base_names[0].split("/", 1)[0]
            if not plugin_root:
                raise ValueError(f"could not determine plugin root folder in {base_plugin_zip}")
            for jar_name in DEVICE_AGENT_JARS:
                entry = f"{plugin_root}/device-agent/{jar_name}"
                if entry in base_names and not (agent_dir / jar_name).is_file():
                    (agent_dir / jar_name).write_bytes(bzf.read(entry))

            missing_jars = [j for j in DEVICE_AGENT_JARS if not (agent_dir / j).is_file()]
            if missing_jars:
                raise FileNotFoundError(f"missing required device-agent DEX jar(s): {', '.join(missing_jars)}")

            # Build universal 3-OS zero-zsh plugin ZIP atomically.
            tmp_plugin = out_dir / f".arb-ide-plugin-{version}.zip.tmp.{os.getpid()}"
            skip_prefixes = (
                f"{plugin_root}/native/",
                f"{plugin_root}/device-agent/",
                f"{plugin_root}/{PLUGIN_LICENSE_DIR}/",
            )
            seen_entries: set[str] = set()
            try:
                with zipfile.ZipFile(tmp_plugin, mode="w", compression=zipfile.ZIP_DEFLATED) as dst:
                    for info in bzf.infolist():
                        if info.filename.startswith(skip_prefixes):
                            continue
                        if info.filename in seen_entries:
                            continue
                        seen_entries.add(info.filename)
                        new_info = zipfile.ZipInfo(filename=info.filename, date_time=info.date_time)
                        new_info.compress_type = info.compress_type
                        new_info.external_attr = info.external_attr
                        new_info.create_system = info.create_system
                        dst.writestr(new_info, bzf.read(info.filename))

                    for jar_name in DEVICE_AGENT_JARS:
                        write_zip_entry(
                            dst,
                            f"{plugin_root}/device-agent/{jar_name}",
                            (agent_dir / jar_name).read_bytes(),
                            executable=False,
                        )

                    for license_name, license_data in license_blobs:
                        write_zip_entry(
                            dst,
                            f"{plugin_root}/{PLUGIN_LICENSE_DIR}/{license_name}",
                            license_data,
                            executable=False,
                        )

                    for (target_os, arch), bin_dir in sorted(extracted_bins.items()):
                        is_unix = target_os != "windows"
                        slug = f"{target_os}-{arch}"
                        for bin_name in BINARIES:
                            fname = binary_filename(bin_name, target_os)
                            write_zip_entry(
                                dst,
                                f"{plugin_root}/native/{slug}/{fname}",
                                (bin_dir / fname).read_bytes(),
                                executable=is_unix,
                            )

                os.replace(tmp_plugin, universal_plugin_zip)
            finally:
                if tmp_plugin.exists():
                    tmp_plugin.unlink(missing_ok=True)

        outputs: list[Path] = [universal_plugin_zip]
        for (target_os, arch), bin_dir in sorted(extracted_bins.items()):
            archive = build_distribution_zip(
                target_os=target_os,
                arch=arch,
                version=version,
                bin_dir=bin_dir,
                plugin_zip=universal_plugin_zip,
                device_agent_dir=agent_dir,
                permissions_doc_path=permissions_doc_path,
                out_dir=out_dir,
                allow_missing_ide_plugin=False,
                allow_missing_device_agent=False,
                license_dir=license_dir,
            )
            outputs.append(archive)

    return outputs


def run_self_test(repo_root: Path) -> int:
    permissions_doc = repo_root / "docs" / "testing" / "first-run-os-permissions.md"
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
            stem = f"arb-{target_os}-{arch}"
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
                            assert "daemon prepare-upgrade" in raw_sh
                    else:
                        raw_ps1 = zf.read(rel).decode("utf-8")
                        assert "\r\n" in raw_ps1 and "arbd.exe" in raw_ps1
                        assert "daemon prepare-upgrade" in raw_ps1

                readme = zf.read(f"{stem}/README-TRIAL.md").decode("utf-8")
                assert "MCP" in readme and "first-run-os-permissions.md" in readme
                assert "GPL / AGPL" in readme
                legacy_ver = "v" + "2"
                assert f"ARB) {legacy_ver}" not in readme
                assert f"arb-{legacy_ver}-" not in readme
                assert f"({legacy_ver})" not in readme
                perm = zf.read(f"{stem}/docs/first-run-os-permissions.md").decode("utf-8")
                assert "Gatekeeper" in perm and "SmartScreen" in perm

                assert_license_files_in_bundle(zf, stem, repo_root)

                sums = zf.read(f"{stem}/SHA256SUMS.txt").decode("utf-8").splitlines()
                assert len(sums) >= 11, f"expected >= 11 checksum lines, got {len(sums)}"
                for line in sums:
                    digest, rel = line.split("  ", 1)
                    assert sha256_bytes(zf.read(rel)) == digest, f"checksum mismatch for {rel}"

        sh_text = render_unix_launcher("linux")
        ps_text = render_windows_launcher()
        assert "stale_pids" not in sh_text and "${cmd%% *}" not in sh_text, "launcher must not kill/parse foreign arbd"
        assert "exit $LASTEXITCODE" in ps_text, "PowerShell must propagate a failed CLI command"
        assert "$stale" not in ps_text, "PS launcher must not kill foreign arbd"

        # Verify universal 3-OS release assembly (CI early + release.yml publish path).
        raw_release = tmp / "raw-release"
        (raw_release / "Linux").mkdir(parents=True)
        (raw_release / "macOS").mkdir(parents=True)
        (raw_release / "Windows").mkdir(parents=True)

        linux_only_plugin = raw_release / "Linux" / "arb-ide-plugin-2.0.0-dev.abcdef0.zip"
        with zipfile.ZipFile(linux_only_plugin, "w", compression=zipfile.ZIP_DEFLATED) as pz:
            write_zip_entry(pz, "arb-ide-plugin/lib/plugin.jar", b"stub-plugin-jar", executable=False)
            for name in BINARIES:
                write_zip_entry(
                    pz,
                    f"arb-ide-plugin/native/linux-x86_64/{name}",
                    f"stub-{name}-linux".encode("utf-8"),
                    executable=True,
                )
            for jar in DEVICE_AGENT_JARS:
                write_zip_entry(
                    pz,
                    f"arb-ide-plugin/device-agent/{jar}",
                    (agent_dir / jar).read_bytes(),
                    executable=False,
                )

        build_distribution_zip(
            target_os="linux",
            arch="x86_64",
            version="2.0.0-dev.abcdef0",
            bin_dir=tmp / "bin-linux",
            plugin_zip=linux_only_plugin,
            device_agent_dir=agent_dir,
            permissions_doc_path=permissions_doc,
            out_dir=raw_release / "Linux",
        )
        build_distribution_zip(
            target_os="macos",
            arch="arm64",
            version="dev-abcdef0",
            bin_dir=tmp / "bin-macos",
            plugin_zip=None,
            device_agent_dir=None,
            permissions_doc_path=permissions_doc,
            out_dir=raw_release / "macOS",
            allow_missing_ide_plugin=True,
            allow_missing_device_agent=True,
        )
        build_distribution_zip(
            target_os="windows",
            arch="x86_64",
            version="dev-abcdef0",
            bin_dir=tmp / "bin-windows",
            plugin_zip=None,
            device_agent_dir=None,
            permissions_doc_path=permissions_doc,
            out_dir=raw_release / "Windows",
            allow_missing_ide_plugin=True,
            allow_missing_device_agent=True,
        )

        assembled_dir = tmp / "assembled"
        assembled = assemble_universal_release(
            input_dir=raw_release,
            out_dir=assembled_dir,
            version="2.0.0-dev.abcdef0",
            permissions_doc_path=permissions_doc,
        )
        assert len(assembled) == 4, f"expected 4 release archives, got {assembled}"
        uni_plugin = assembled_dir / "arb-ide-plugin-2.0.0-dev.abcdef0.zip"
        assert uni_plugin.is_file(), "missing assembled universal plugin ZIP"
        with zipfile.ZipFile(uni_plugin, "r") as uzf:
            unames = uzf.namelist()
            assert len(unames) == len(set(unames)), f"duplicate entries in universal plugin ZIP: {unames}"
            assert "arb-ide-plugin/lib/plugin.jar" in unames
            for jar in DEVICE_AGENT_JARS:
                assert f"arb-ide-plugin/device-agent/{jar}" in unames
            for lic in LICENSE_FILES:
                rel = f"arb-ide-plugin/{PLUGIN_LICENSE_DIR}/{lic}"
                assert rel in unames, f"missing {rel} in universal plugin ZIP"
                assert uzf.read(rel) == (repo_root / lic).read_bytes(), f"{rel} differs from repository {lic}"
            for target_os, arch, _ in matrix:
                for name in BINARIES:
                    rel = f"arb-ide-plugin/native/{target_os}-{arch}/{binary_filename(name, target_os)}"
                    assert rel in unames, f"missing {rel} in universal plugin ZIP"
                    if target_os != "windows":
                        mode = (uzf.getinfo(rel).external_attr >> 16) & 0o777
                        assert mode == 0o755, f"expected 0755 on {rel}, got {oct(mode)}"

        for target_os, arch, _ in matrix:
            stem = f"arb-{target_os}-{arch}"
            bzip = assembled_dir / f"{stem}.zip"
            assert bzip.is_file(), f"missing assembled bundle {bzip}"
            with zipfile.ZipFile(bzip, "r") as bzf:
                bnames = set(bzf.namelist())
                assert f"{stem}/{uni_plugin.name}" in bnames, f"missing universal plugin in {bzip}"
                for jar in DEVICE_AGENT_JARS:
                    assert f"{stem}/device-agent/{jar}" in bnames, f"missing {jar} in {bzip}"
                readme = bzf.read(f"{stem}/README-TRIAL.md").decode("utf-8")
                assert "2.0.0-dev.abcdef0" in readme
                assert_license_files_in_bundle(bzf, stem, repo_root)
                sum_names = {
                    line.split("  ", 1)[1] for line in bzf.read(f"{stem}/SHA256SUMS.txt").decode("utf-8").splitlines()
                }
                for lic in LICENSE_FILES:
                    assert f"{stem}/{lic}" in sum_names, f"{lic} missing from SHA256SUMS.txt of {bzip}"
                for line in bzf.read(f"{stem}/SHA256SUMS.txt").decode("utf-8").splitlines():
                    digest, rel = line.split("  ", 1)
                    assert sha256_bytes(bzf.read(rel)) == digest, f"checksum mismatch for {rel} in {bzip}"

        run_binary_identity_self_test(tmp)
        run_release_tag_self_test(tmp)
        run_core_version_self_test(tmp, repo_root)

        print(
            "package_distribution.py --self-test: OK "
            "(verified linux-x86_64, macos-arm64, windows-x86_64 archives, universal 3-OS plugin assembly, POSIX modes, UTF-8 docs, SHA256SUMS, "
            "same-size build digest identity, license files + SHA256SUMS + no retired version label, release tag classification, "
            "shared launcher upgrade command)"
        )
    return 0


def assert_license_files_in_bundle(zf: zipfile.ZipFile, stem: str, repo_root: Path) -> None:
    """G-M1: every OS bundle carries the dual license + third-party notices, listed in SHA256SUMS.txt,
    byte-identical to the repository root files, without the retired product-version label."""
    names = set(zf.namelist())
    sums = {
        line.split("  ", 1)[1]: line.split("  ", 1)[0]
        for line in zf.read(f"{stem}/SHA256SUMS.txt").decode("utf-8").splitlines()
    }
    legacy_label = "v" + "2"
    for lic in LICENSE_FILES:
        rel = f"{stem}/{lic}"
        assert rel in names, f"missing {rel}"
        data = zf.read(rel)
        assert data == (repo_root / lic).read_bytes(), f"{rel} differs from repository {lic}"
        assert sums.get(rel) == sha256_bytes(data), f"{rel} missing or stale in SHA256SUMS.txt"
        text = data.decode("utf-8")
        assert not re.search(rf"\b{legacy_label}\b", text, re.IGNORECASE), f"{rel} contains the retired '{legacy_label}' label"
    mit = zf.read(f"{stem}/LICENSE-MIT").decode("utf-8")
    assert "MIT License" in mit and "Copyright (c) 2026 shinma06" in mit
    apache = zf.read(f"{stem}/LICENSE-APACHE").decode("utf-8")
    assert "Apache License" in apache and "Version 2.0, January 2004" in apache
    notices = zf.read(f"{stem}/THIRD_PARTY_NOTICES.md").decode("utf-8")
    for needle in ("scrcpy-server", "v5.0.1", "openh264", "BSD-2-Clause", "rusty_aac", "rusqlite", "check-licenses.py", "Copyright (c) 2013, Cisco Systems", "Redistributions in binary form must reproduce"):
        assert needle.lower() in notices.lower(), f"THIRD_PARTY_NOTICES.md must mention {needle}"
    readme = zf.read(f"{stem}/README-TRIAL.md").decode("utf-8")
    assert "MIT OR Apache-2.0" in readme and "THIRD_PARTY_NOTICES.md" in readme


def run_binary_identity_self_test(tmp: Path) -> None:
    """M9: reproducible ZIPs give two builds that differ only in constants the same size and the
    same entry timestamp, so size + mtime cannot identify the bundled `arbd`; SHA256SUMS (and the
    IDE's content digest of the extracted binary) must still tell them apart."""
    infos = []
    for label, payload in (("old", b"arbd-const=1"), ("new", b"arbd-const=2")):
        archive = tmp / f"identity-{label}.zip"
        with zipfile.ZipFile(archive, "w") as zf:
            write_zip_entry(zf, "bin/arbd", payload, executable=True)
        with zipfile.ZipFile(archive) as zf:
            info = zf.getinfo("bin/arbd")
            infos.append((info.file_size, info.date_time, sha256_bytes(zf.read("bin/arbd"))))
    (old_size, old_time, old_digest), (new_size, new_time, new_digest) = infos
    assert old_size == new_size, "fixture builds must have identical size"
    assert old_time == new_time == ZIP_TIMESTAMP, "reproducible ZIP entries share a fixed timestamp"
    assert old_digest != new_digest, "content digest must distinguish same-size, same-mtime builds"


def run_core_version_self_test(tmp: Path, repo_root: Path) -> None:
    """Tagged releases must match `core/Cargo.toml` so `arbd --version` never misreports the release."""
    fake = tmp / "fake-repo"
    (fake / "core").mkdir(parents=True)
    (fake / "core" / "Cargo.toml").write_text(
        '[workspace]\nresolver = "2"\n\n[workspace.package]\nversion = "2.0.0-dev"  # dev\nedition = "2024"\n\n'
        '[workspace.dependencies]\nserde = { version = "1.0.0" }\n',
        encoding="utf-8",
    )
    assert read_workspace_version(fake / "core" / "Cargo.toml") == "2.0.0-dev"
    assert check_core_version(fake, "2.0.0-dev") is None
    assert check_core_version(fake, "2.0.0") is not None, "v2.0.0 must be rejected while Cargo.toml says 2.0.0-dev"
    real = read_workspace_version(repo_root / "core" / "Cargo.toml")
    assert real is not None and not real.startswith("0."), f"core/Cargo.toml version looks stale: {real}"


def run_release_tag_self_test(tmp: Path) -> None:
    """M12: only `vX.Y.Z` is a stable `--latest` release; SemVer prereleases never are."""
    expectations = {
        "v2.0.0": ("stable", False, ["--latest"]),
        "v10.20.30": ("stable", False, ["--latest"]),
        "v2.1.0-rc.1": ("prerelease", True, ["--prerelease", "--latest=false"]),
        "v2.1.0-beta.3": ("prerelease", True, ["--prerelease", "--latest=false"]),
        "v2.1.0-alpha.0": ("prerelease", True, ["--prerelease", "--latest=false"]),
    }
    for tag, (channel, prerelease, flags) in expectations.items():
        got = classify_release_tag(tag)
        assert got["channel"] == channel, f"{tag}: channel {got['channel']} != {channel}"
        assert got["prerelease"] is prerelease, f"{tag}: prerelease flag mismatch"
        assert got["gh_release_flags"] == flags, f"{tag}: gh flags {got['gh_release_flags']} != {flags}"
        assert got["version"] == tag[1:], f"{tag}: version mismatch"
    for bad in ("v2.0", "2.0.0", "v02.0.0", "v2.0.0-rc", "v2.0.0-dev.1", "v2.0.0+build", "v2-early", "v2.0.0-rc.01", ""):
        try:
            classify_release_tag(bad)
        except ValueError:
            continue
        raise AssertionError(f"{bad!r} must be rejected")
    out = tmp / "github-output.txt"
    write_tag_classification(classify_release_tag("v2.1.0-rc.1"), out, echo=False)
    text = out.read_text(encoding="utf-8")
    assert "prerelease=true" in text and "gh_release_flags=--prerelease --latest=false" in text, text
    assert "version=2.1.0-rc.1" in text, text


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Package self-contained 3-OS distribution ZIP for Android Replay Buffer."
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run cross-OS packaging self-test across linux, macos, and windows fixtures.",
    )
    parser.add_argument(
        "--assemble-release",
        action="store_true",
        help="Assemble universal 3-OS plugin ZIP and symmetric 3-OS bundles from CI/release artifacts.",
    )
    parser.add_argument(
        "--input-dir",
        type=Path,
        default=None,
        help="Input directory containing per-OS arb-*.zip bundles and base arb-ide-plugin-*.zip (for --assemble-release).",
    )
    parser.add_argument(
        "--classify-tag",
        default=None,
        help="Classify a release tag (vX.Y.Z stable vs vX.Y.Z-{alpha,beta,rc}.N prerelease) and print gh flags.",
    )
    parser.add_argument(
        "--check-core-version",
        default=None,
        metavar="VERSION",
        help="Fail unless core/Cargo.toml [workspace.package] version equals VERSION (release tag consistency).",
    )
    parser.add_argument(
        "--github-output",
        type=Path,
        default=None,
        help="Append --classify-tag results to this GitHub Actions output file.",
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
    if args.check_core_version is not None:
        error = check_core_version(repo_root, args.check_core_version)
        if error:
            print(f"ERROR: {error}", file=sys.stderr)
            return 1
        print(f"core/Cargo.toml version matches {args.check_core_version}")
        return 0
    if args.classify_tag is not None:
        try:
            classification = classify_release_tag(args.classify_tag)
        except ValueError as err:
            print(f"ERROR: {err}", file=sys.stderr)
            return 1
        write_tag_classification(classification, args.github_output)
        return 0

    permissions_doc = repo_root / "docs" / "testing" / "first-run-os-permissions.md"
    if args.assemble_release:
        if args.input_dir is None:
            parser.error("--assemble-release requires --input-dir")
        version = args.version or detect_version(repo_root)
        out_dir = args.out_dir or (repo_root / "dist")
        outputs = assemble_universal_release(
            input_dir=args.input_dir,
            out_dir=out_dir,
            version=version,
            permissions_doc_path=permissions_doc,
        )
        for path in outputs:
            print(f"Assembled {path} ({path.stat().st_size} bytes)")
        return 0

    target_os = normalize_os(args.target_os)
    arch = normalize_arch(args.arch)
    version = args.version or detect_version(repo_root)
    bin_dir = args.bin_dir or (repo_root / "core" / "target" / "release")
    plugin_zip = resolve_plugin_zip(args.plugin_zip, repo_root)
    device_agent_dir = args.device_agent_dir or (repo_root / "device-agent" / "build")
    out_dir = args.out_dir or (repo_root / "dist")

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

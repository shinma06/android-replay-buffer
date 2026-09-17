"""Configuration loading for replay buffer."""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any

DEFAULT_CONFIG_PATHS = (
    Path.home() / ".config" / "replay-buffer" / "config.json",
    Path.home() / ".config" / "replay-buffer" / "config.yaml",
    Path(__file__).resolve().parent.parent / "config.json",
)


@dataclass(frozen=True)
class Config:
    replay_seconds: int = 60
    output_directory: Path = Path.home() / "Replays"
    segment_seconds: int = 5
    state_directory: Path = Path.home() / ".replay-buffer"
    adb_path: str = "adb"
    scrcpy_path: str = "scrcpy"
    ffmpeg_path: str = "ffmpeg"

    @property
    def socket_path(self) -> Path:
        return self.state_directory / "replayd.sock"

    @property
    def pid_path(self) -> Path:
        return self.state_directory / "replayd.pid"


def _coerce_path(value: Any, default: Path) -> Path:
    if value is None:
        return default
    return Path(os.path.expanduser(str(value)))


def _load_file_data(path: Path) -> dict[str, Any]:
    text = path.read_text(encoding="utf-8")
    if path.suffix in {".yaml", ".yml"}:
        return _parse_simple_yaml(text)
    return json.loads(text)


def _parse_simple_yaml(text: str) -> dict[str, Any]:
    """Parse the small flat YAML subset used by this tool."""
    data: dict[str, Any] = {}
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if ":" not in stripped:
            continue
        key, value = stripped.split(":", 1)
        key = key.strip()
        value = value.strip()
        if not key:
            continue
        if value.isdigit():
            data[key] = int(value)
        else:
            data[key] = value.strip("'\"")
    return data


def load_config(path: Path | None = None) -> Config:
    """Load config from explicit path or default locations."""
    config_path = path
    if config_path is None:
        for candidate in DEFAULT_CONFIG_PATHS:
            if candidate.is_file():
                config_path = candidate
                break

    data: dict[str, Any] = {}
    if config_path is not None and config_path.is_file():
        loaded = _load_file_data(config_path)
        if not isinstance(loaded, dict):
            raise ValueError(f"Invalid config format: {config_path}")
        data = loaded

    replay_seconds = int(data.get("replay_seconds", 60))
    if replay_seconds <= 0:
        raise ValueError("replay_seconds must be positive")

    segment_seconds = int(data.get("segment_seconds", 5))
    if segment_seconds <= 0:
        raise ValueError("segment_seconds must be positive")

    return Config(
        replay_seconds=replay_seconds,
        output_directory=_coerce_path(
            data.get("output_directory"), Path.home() / "Replays"
        ),
        segment_seconds=segment_seconds,
        state_directory=_coerce_path(
            data.get("state_directory"), Path.home() / ".replay-buffer"
        ),
        adb_path=str(data.get("adb_path", "adb")),
        scrcpy_path=str(data.get("scrcpy_path", "scrcpy")),
        ffmpeg_path=str(data.get("ffmpeg_path", "ffmpeg")),
    )

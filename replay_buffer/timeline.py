"""Timeline event model and serialization."""

from __future__ import annotations

import json
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


@dataclass
class TimelineEvent:
    replay_time_ms: int
    host_time_ms: float
    device_time_ms: int | None
    source: str
    level: str | None
    tag: str | None
    message: str

    def to_dict(self) -> dict[str, Any]:
        payload = asdict(self)
        payload["replay_time_ms"] = int(payload["replay_time_ms"])
        payload["host_time_ms"] = round(float(payload["host_time_ms"]), 3)
        if payload["device_time_ms"] is not None:
            payload["device_time_ms"] = int(payload["device_time_ms"])
        return payload


def monotonic_ms() -> float:
    return time.monotonic() * 1000.0


def build_timeline(
    events: list[TimelineEvent],
    save_host_time_ms: float,
    replay_seconds: int,
    device_serial: str,
    device_name: str,
    video_path: str,
    logcat_path: str,
) -> dict[str, Any]:
    """Build timeline.json payload."""
    sorted_events = sorted(events, key=lambda event: event.host_time_ms)
    return {
        "version": 1,
        "saved_at_unix_ms": int(time.time() * 1000),
        "save_host_time_ms": round(save_host_time_ms, 3),
        "replay_seconds": replay_seconds,
        "device": {
            "serial": device_serial,
            "name": device_name,
        },
        "artifacts": {
            "video": video_path,
            "logcat": logcat_path,
        },
        "events": [event.to_dict() for event in sorted_events],
    }


def write_timeline(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, ensure_ascii=False, indent=2)
        handle.write("\n")

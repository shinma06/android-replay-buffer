"""ADB device detection and subprocess helpers."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass


@dataclass(frozen=True)
class AdbDevice:
    serial: str
    state: str
    product: str | None = None
    model: str | None = None
    device: str | None = None

    @property
    def display_name(self) -> str:
        for candidate in (self.model, self.product, self.device, self.serial):
            if candidate:
                return candidate
        return self.serial


_DEVICE_LINE = re.compile(r"^(\S+)\s+(device|unauthorized|offline|no permissions)\b")


def run_adb(
    adb_path: str,
    *args: str,
    serial: str | None = None,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    command = [adb_path]
    if serial:
        command.extend(["-s", serial])
    command.extend(args)
    return subprocess.run(
        command,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def list_devices(adb_path: str) -> list[AdbDevice]:
    """Return connected ADB devices with optional `-l` metadata."""
    result = run_adb(adb_path, "devices", "-l")
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "adb devices failed")

    devices: list[AdbDevice] = []
    for line in result.stdout.splitlines():
        match = _DEVICE_LINE.match(line.strip())
        if not match:
            continue
        serial, state = match.group(1), match.group(2)
        metadata = _parse_device_metadata(line)
        devices.append(
            AdbDevice(
                serial=serial,
                state=state,
                product=metadata.get("product"),
                model=metadata.get("model"),
                device=metadata.get("device"),
            )
        )
    return devices


def pick_active_device(adb_path: str) -> AdbDevice | None:
    """Pick the first authorized device, if any."""
    for device in list_devices(adb_path):
        if device.state == "device":
            return device
    return None


def get_device_model(adb_path: str, serial: str) -> str | None:
    result = run_adb(adb_path, "shell", "getprop", "ro.product.model", serial=serial)
    if result.returncode != 0:
        return None
    value = result.stdout.strip()
    return value or None


def _parse_device_metadata(line: str) -> dict[str, str]:
    metadata: dict[str, str] = {}
    for token in line.split():
        if ":" not in token:
            continue
        key, value = token.split(":", 1)
        metadata[key] = value
    return metadata

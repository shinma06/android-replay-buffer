"""Rolling logcat capture with timeline events."""

from __future__ import annotations

import re
import subprocess
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import Callable

from replay_buffer.timeline import TimelineEvent, monotonic_ms

# threadtime: MM-DD HH:MM:SS.mmm  pid tid LEVEL tag: message
_LOGCAT_LINE = re.compile(
    r"^(?P<date>\d{2}-\d{2})\s+"
    r"(?P<time>\d{2}:\d{2}:\d{2}\.\d{3})\s+"
    r"(?P<pid>\d+)\s+"
    r"(?P<tid>\d+)\s+"
    r"(?P<level>[VDIWEF])\s+"
    r"(?P<tag>[^:]+?):\s+"
    r"(?P<message>.*)$"
)


@dataclass
class LogRecord:
    host_time_ms: float
    raw: str
    event: TimelineEvent | None


class LogBuffer:
    """Capture adb logcat into a time-bounded ring buffer."""

    def __init__(
        self,
        adb_path: str,
        serial: str,
        replay_seconds: int,
        on_status: Callable[[], None] | None = None,
    ) -> None:
        self._adb_path = adb_path
        self._serial = serial
        self._replay_seconds = replay_seconds
        self._on_status = on_status
        self._records: deque[LogRecord] = deque()
        self._lock = threading.Lock()
        self._process: subprocess.Popen[bytes] | None = None
        self._reader_thread: threading.Thread | None = None
        self._stop_event = threading.Event()

    def start(self) -> None:
        command = [
            self._adb_path,
            "-s",
            self._serial,
            "logcat",
            "-v",
            "threadtime",
        ]
        self._process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self._reader_thread = threading.Thread(
            target=self._read_loop,
            name=f"logcat-{self._serial}",
            daemon=True,
        )
        self._reader_thread.start()

    def stop(self) -> None:
        self._stop_event.set()
        process = self._process
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        if self._reader_thread:
            self._reader_thread.join(timeout=3)

    def records_in_window(
        self, window_start_ms: float, window_end_ms: float
    ) -> list[LogRecord]:
        with self._lock:
            return [
                record
                for record in self._records
                if window_start_ms <= record.host_time_ms <= window_end_ms
            ]

    def _read_loop(self) -> None:
        process = self._process
        if process is None or process.stdout is None:
            return

        while not self._stop_event.is_set():
            line_bytes = process.stdout.readline()
            if not line_bytes:
                break
            raw = line_bytes.decode("utf-8", errors="replace").rstrip("\r\n")
            if not raw:
                continue
            host_time_ms = monotonic_ms()
            event = _parse_logcat_line(raw, host_time_ms)
            record = LogRecord(host_time_ms=host_time_ms, raw=raw, event=event)
            with self._lock:
                self._records.append(record)
                self._prune_locked(host_time_ms)
            if self._on_status:
                self._on_status()

        stderr = ""
        if process.stderr is not None:
            stderr = process.stderr.read().decode("utf-8", errors="replace")
        if not self._stop_event.is_set() and process.poll() not in (0, None):
            message = stderr.strip() or f"logcat exited with code {process.returncode}"
            raise RuntimeError(message)

    def _prune_locked(self, now_ms: float) -> None:
        cutoff = now_ms - (self._replay_seconds * 1000)
        while self._records and self._records[0].host_time_ms < cutoff:
            self._records.popleft()


def _parse_logcat_line(raw: str, host_time_ms: float) -> TimelineEvent | None:
    match = _LOGCAT_LINE.match(raw)
    if not match:
        return None

    device_time_ms = _device_time_to_epoch_ms(
        match.group("date"), match.group("time")
    )
    return TimelineEvent(
        replay_time_ms=0,
        host_time_ms=host_time_ms,
        device_time_ms=device_time_ms,
        source="logcat",
        level=match.group("level"),
        tag=match.group("tag").strip(),
        message=match.group("message"),
    )


def _device_time_to_epoch_ms(date_part: str, time_part: str) -> int | None:
    """Best-effort conversion using current year."""
    year = time.localtime().tm_year
    try:
        parsed = time.strptime(f"{year}-{date_part} {time_part}", "%Y-%m-%d %H:%M:%S")
    except ValueError:
        return None
    fractional = time_part.split(".", 1)
    millis = int(fractional[1]) if len(fractional) == 2 else 0
    return int(time.mktime(parsed) * 1000) + millis

"""Active capture session for one Android device."""

from __future__ import annotations

import threading
import time
from pathlib import Path

from replay_buffer.adb import AdbDevice, get_device_model
from replay_buffer.config import Config
from replay_buffer.log_buffer import LogBuffer
from replay_buffer.save import save_replay
from replay_buffer.timeline import monotonic_ms
from replay_buffer.video_buffer import VideoBuffer


class ReplaySession:
    """Manage synchronized video and logcat capture for one device."""

    def __init__(self, config: Config, device: AdbDevice) -> None:
        self.config = config
        self.device = device
        self.device_name = device.display_name
        self.work_dir = config.state_directory / "sessions" / device.serial
        self.started_at_ms = monotonic_ms()
        self.error: str | None = None

        self._video_buffer = VideoBuffer(
            adb_path=config.adb_path,
            scrcpy_path=config.scrcpy_path,
            ffmpeg_path=config.ffmpeg_path,
            serial=device.serial,
            work_dir=self.work_dir,
            replay_seconds=config.replay_seconds,
            segment_seconds=config.segment_seconds,
        )
        self._log_buffer = LogBuffer(
            adb_path=config.adb_path,
            serial=device.serial,
            replay_seconds=config.replay_seconds,
        )
        self._lock = threading.Lock()
        self._started = False

    def start(self) -> None:
        model = get_device_model(self.config.adb_path, self.device.serial)
        if model:
            self.device_name = model

        self.work_dir.mkdir(parents=True, exist_ok=True)
        self._video_buffer.start()
        self._log_buffer.start()
        self._started = True

    def stop(self) -> None:
        with self._lock:
            if not self._started:
                return
            self._log_buffer.stop()
            self._video_buffer.stop()
            self._started = False

    def save(self) -> Path:
        with self._lock:
            if not self._started:
                raise RuntimeError("No active recording session")
            if self.error:
                raise RuntimeError(self.error)
        return save_replay(
            video_buffer=self._video_buffer,
            log_buffer=self._log_buffer,
            ffmpeg_path=self.config.ffmpeg_path,
            output_directory=self.config.output_directory,
            replay_seconds=self.config.replay_seconds,
            device_serial=self.device.serial,
            device_name=self.device_name,
        )

    def status(self) -> dict[str, object]:
        uptime_sec = max(0.0, (monotonic_ms() - self.started_at_ms) / 1000.0)
        error = self.error or self._video_buffer.error
        return {
            "device_serial": self.device.serial,
            "device_name": self.device_name,
            "recording": self._started and error is None,
            "replay_seconds": self.config.replay_seconds,
            "uptime_seconds": round(uptime_sec, 1),
            "error": error,
        }


class SessionManager:
    """Start/stop sessions as ADB devices connect and disconnect."""

    def __init__(self, config: Config) -> None:
        self.config = config
        self._session: ReplaySession | None = None
        self._lock = threading.Lock()

    @property
    def session(self) -> ReplaySession | None:
        with self._lock:
            return self._session

    def on_device_connected(self, device: AdbDevice) -> ReplaySession:
        with self._lock:
            if self._session and self._session.device.serial == device.serial:
                return self._session
            previous = self._session
            self._session = None

        if previous is not None:
            previous.stop()
            time.sleep(0.2)

        session = ReplaySession(self.config, device)
        try:
            session.start()
        except Exception as exc:  # noqa: BLE001 - keep daemon alive
            session.error = str(exc)
            session.stop()
            raise

        with self._lock:
            if self._session and self._session.device.serial != device.serial:
                session.stop()
                return self._session
            self._session = session
        return session

    def on_device_disconnected(self, serial: str) -> None:
        with self._lock:
            if self._session is None or self._session.device.serial != serial:
                return
            session = self._session
            self._session = None
        session.stop()
        time.sleep(0.2)

    def stop_all(self) -> None:
        with self._lock:
            session = self._session
            self._session = None
        if session is not None:
            session.stop()
            time.sleep(0.2)

    def save(self) -> Path:
        with self._lock:
            if self._session is None:
                raise RuntimeError("No Android device connected")
            session = self._session
        return session.save()

    def status(self) -> dict[str, object]:
        with self._lock:
            session = self._session
        if session is None:
            return {
                "state": "waiting",
                "recording": False,
                "replay_seconds": self.config.replay_seconds,
            }
        payload = session.status()
        payload["state"] = "recording" if session.error is None else "error"
        return payload


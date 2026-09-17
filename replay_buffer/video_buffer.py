"""Rolling video capture using scrcpy segment files."""

from __future__ import annotations

import shutil
import subprocess
import threading
import time
from dataclasses import dataclass
from pathlib import Path

from replay_buffer.timeline import monotonic_ms


@dataclass
class VideoSegment:
    path: Path
    index: int
    start_host_time_ms: float
    end_host_time_ms: float | None = None

    def overlaps_window(self, window_start_ms: float, window_end_ms: float) -> bool:
        end_ms = self.end_host_time_ms
        if end_ms is None:
            end_ms = monotonic_ms()
        return self.start_host_time_ms <= window_end_ms and end_ms >= window_start_ms


class VideoBuffer:
    """Capture Android screen into rolling MKV segments via scrcpy."""

    def __init__(
        self,
        adb_path: str,
        scrcpy_path: str,
        ffmpeg_path: str,
        serial: str,
        work_dir: Path,
        replay_seconds: int,
        segment_seconds: int,
    ) -> None:
        self._adb_path = adb_path
        self._scrcpy_path = scrcpy_path
        self._ffmpeg_path = ffmpeg_path
        self._serial = serial
        self._work_dir = work_dir
        self._replay_seconds = replay_seconds
        self._segment_seconds = segment_seconds
        self._segments_dir = work_dir / "segments"
        self._segments: list[VideoSegment] = []
        self._lock = threading.Lock()
        self._scrcpy_process: subprocess.Popen[bytes] | None = None
        self._record_thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self.error: str | None = None

    def start(self) -> None:
        if shutil.which(self._scrcpy_path) is None:
            raise RuntimeError(
                f"scrcpy not found: {self._scrcpy_path}. Install with: brew install scrcpy"
            )

        self._segments_dir.mkdir(parents=True, exist_ok=True)
        for stale in self._segments_dir.glob("seg_*.*"):
            stale.unlink(missing_ok=True)

        self._record_thread = threading.Thread(
            target=self._record_loop,
            name=f"video-buffer-{self._serial}",
            daemon=True,
        )
        self._record_thread.start()

    def stop(self) -> None:
        self._stop_event.set()
        process = self._scrcpy_process
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        if self._record_thread:
            self._record_thread.join(timeout=self._segment_seconds + 5)

    def segments_in_window(
        self, window_start_ms: float, window_end_ms: float
    ) -> list[VideoSegment]:
        self._sync_segments_from_disk()
        with self._lock:
            return [
                segment
                for segment in self._segments
                if segment.overlaps_window(window_start_ms, window_end_ms)
            ]

    def _sync_segments_from_disk(self) -> None:
        """Register completed segment files that may not yet be in memory."""
        paths = sorted(self._segments_dir.glob("seg_*.mkv"))
        if not paths:
            return

        with self._lock:
            known = {segment.path.name for segment in self._segments}
            segment_duration_ms = self._segment_seconds * 1000
            for path in paths:
                if path.name in known or path.stat().st_size == 0:
                    continue
                index = int(path.stem.split("_", 1)[1])
                end_ms = path.stat().st_mtime * 1000.0
                start_ms = end_ms - segment_duration_ms
                self._segments.append(
                    VideoSegment(
                        path=path,
                        index=index,
                        start_host_time_ms=start_ms,
                        end_host_time_ms=end_ms,
                    )
                )
            self._prune_locked(monotonic_ms())

    def _record_loop(self) -> None:
        index = 0
        while not self._stop_event.is_set():
            path = self._segments_dir / f"seg_{index:06d}.mkv"
            start_ms = monotonic_ms()
            command = [
                self._scrcpy_path,
                "--serial",
                self._serial,
                "--no-playback",
                "--no-control",
                "--no-audio",
                "--record",
                str(path),
                "--record-format",
                "mkv",
                "--time-limit",
                str(self._segment_seconds),
            ]
            process = subprocess.Popen(
                command,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            self._scrcpy_process = process
            try:
                returncode = process.wait()
            except Exception as exc:  # noqa: BLE001 - keep loop alive
                self.error = str(exc)
                return

            stderr = ""
            if process.stderr is not None:
                stderr = process.stderr.read().decode("utf-8", errors="replace")

            if self._stop_event.is_set():
                path.unlink(missing_ok=True)
                break

            if returncode != 0:
                self.error = stderr.strip() or f"scrcpy exited with code {returncode}"
                return

            if path.is_file() and path.stat().st_size > 0:
                end_ms = monotonic_ms()
                with self._lock:
                    self._segments.append(
                        VideoSegment(
                            path=path,
                            index=index,
                            start_host_time_ms=start_ms,
                            end_host_time_ms=end_ms,
                        )
                    )
                    self._prune_locked(end_ms)
            else:
                path.unlink(missing_ok=True)

            index += 1
            time.sleep(0.05)

    def _prune_locked(self, now_ms: float) -> None:
        cutoff = now_ms - (self._replay_seconds * 1000)
        kept: list[VideoSegment] = []
        for segment in self._segments:
            end_ms = segment.end_host_time_ms or now_ms
            if end_ms >= cutoff:
                kept.append(segment)
            else:
                segment.path.unlink(missing_ok=True)
        self._segments = kept

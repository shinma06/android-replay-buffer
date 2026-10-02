"""Persist replay window to disk."""

from __future__ import annotations

import subprocess
import time
from datetime import datetime
from pathlib import Path

from replay_buffer.log_buffer import LogBuffer, LogRecord
from replay_buffer.timeline import TimelineEvent, build_timeline, write_timeline
from replay_buffer.video_buffer import VideoBuffer, VideoSegment


def save_replay(
    *,
    video_buffer: VideoBuffer,
    log_buffer: LogBuffer,
    ffmpeg_path: str,
    output_directory: Path,
    replay_seconds: int,
    device_serial: str,
    device_name: str,
    save_host_time_ms: float | None = None,
) -> tuple[Path, bool]:
    """Save the last N seconds of video and logcat with timeline metadata."""
    if save_host_time_ms is None:
        save_host_time_ms = time.monotonic() * 1000.0

    window_start_ms = save_host_time_ms - (replay_seconds * 1000)
    window_end_ms = save_host_time_ms

    segments = video_buffer.segments_in_window(window_start_ms, window_end_ms)
    log_records = log_buffer.records_in_window(window_start_ms, window_end_ms)

    if not segments and not log_records:
        raise RuntimeError(
            "No replay data in buffer yet. "
            "Wait a few seconds after connecting, then run replay save again."
        )

    timestamp = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
    replay_dir = output_directory / timestamp
    replay_dir.mkdir(parents=True, exist_ok=True)

    video_path = replay_dir / "replay.mp4"
    logcat_path = replay_dir / "logcat.txt"
    timeline_path = replay_dir / "timeline.json"

    _write_logcat(logcat_path, log_records)
    video_saved = _write_video(ffmpeg_path, video_path, segments)
    events = _build_events(log_records, save_host_time_ms)
    timeline = build_timeline(
        events=events,
        save_host_time_ms=save_host_time_ms,
        replay_seconds=replay_seconds,
        device_serial=device_serial,
        device_name=device_name,
        video_path=video_path.name if video_saved else None,
        logcat_path=logcat_path.name,
    )
    write_timeline(timeline_path, timeline)
    return replay_dir, video_saved


def _write_logcat(path: Path, records: list[LogRecord]) -> None:
    with path.open("w", encoding="utf-8") as handle:
        for record in records:
            handle.write(record.raw)
            handle.write("\n")


def _write_video(
    ffmpeg_path: str, output_path: Path, segments: list[VideoSegment]
) -> bool:
    usable = [
        segment
        for segment in segments
        if segment.path.is_file() and segment.path.stat().st_size > 0
    ]
    if not usable:
        return False

    ordered = sorted(usable, key=lambda segment: segment.index)
    concat_file = output_path.with_suffix(".concat.txt")
    temp_mkv = output_path.with_suffix(".tmp.mkv")
    with concat_file.open("w", encoding="utf-8") as handle:
        for segment in ordered:
            escaped = segment.path.as_posix().replace("'", "'\\''")
            handle.write(f"file '{escaped}'\n")

    concat_command = [
        ffmpeg_path,
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        str(concat_file),
        "-c",
        "copy",
        str(temp_mkv),
    ]
    concat_result = subprocess.run(
        concat_command, capture_output=True, text=True, check=False, timeout=60
    )
    concat_file.unlink(missing_ok=True)
    if concat_result.returncode != 0:
        temp_mkv.unlink(missing_ok=True)
        raise RuntimeError(concat_result.stderr.strip() or "ffmpeg concat failed")

    remux_command = [
        ffmpeg_path,
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        str(temp_mkv),
        "-c",
        "copy",
        "-movflags",
        "+faststart",
        str(output_path),
    ]
    remux_result = subprocess.run(
        remux_command, capture_output=True, text=True, check=False, timeout=60
    )
    temp_mkv.unlink(missing_ok=True)
    if remux_result.returncode != 0:
        raise RuntimeError(remux_result.stderr.strip() or "ffmpeg remux failed")
    return True


def _build_events(
    records: list[LogRecord], save_host_time_ms: float
) -> list[TimelineEvent]:
    events: list[TimelineEvent] = []
    for record in records:
        if record.event is None:
            continue
        replay_time_ms = int(round(record.host_time_ms - save_host_time_ms))
        events.append(
            TimelineEvent(
                replay_time_ms=replay_time_ms,
                host_time_ms=record.host_time_ms,
                device_time_ms=record.event.device_time_ms,
                source=record.event.source,
                level=record.event.level,
                tag=record.event.tag,
                message=record.event.message,
            )
        )

    events.append(
        TimelineEvent(
            replay_time_ms=0,
            host_time_ms=save_host_time_ms,
            device_time_ms=int(time.time() * 1000),
            source="replay",
            level=None,
            tag=None,
            message="[SAVE]",
        )
    )
    return events

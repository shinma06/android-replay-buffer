package io.github.shinma06.replaybuffer.core

import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.util.UUID

/** File names inside one saved replay folder. */
internal object ReplayFiles {
    const val SUMMARY = "README.md"
    const val RECORD = "replay.json"
    const val VIDEO = "video.mp4"
    const val VIDEO_FRAMES = "video-frames.jsonl"
    const val VIDEO_SOURCE = "video-source.h264"
    const val LOGCAT_DEVICE = "logcat-device.jsonl"
    const val LOGCAT_APP = "logcat-app.jsonl"
    const val TOUCHES = "touches.jsonl"
    const val SCHEMA = 4
}

private val NAME_TIME: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")

/** Eight random hex digits per save attempt: a retried capture keeps its save_id but gets a new folder. */
internal fun nameId(): String = UUID.randomUUID().toString().take(8)

/**
 * `replay-<local yyyyMMdd-HHmmss>-<manual|crash>[-logs]-<id>`: sorts by local time, uses only `[a-z0-9-]` on every OS,
 * and never matches a Windows reserved name.
 */
internal fun replayFolderName(at: ZonedDateTime, crash: Boolean, logsOnly: Boolean, id: String = nameId()): String =
    "replay-${at.format(NAME_TIME)}-${if (crash) "crash" else "manual"}${if (logsOnly) "-logs" else ""}-$id"

/** `screenshot-<local yyyyMMdd-HHmmss>-<id>.png`, the same time and ID format as saved replay folders. */
internal fun screenshotFileName(at: ZonedDateTime, id: String = nameId()): String = "screenshot-${at.format(NAME_TIME)}-$id.png"

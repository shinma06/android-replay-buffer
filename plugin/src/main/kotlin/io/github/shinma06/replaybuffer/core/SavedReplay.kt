package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonObject
import java.nio.channels.Channels
import java.nio.channels.FileChannel
import java.nio.channels.ReadableByteChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ

/** A completed schema-4 saved replay folder opened read-only through the save handles. */
internal class SavedReplay private constructor(
    val directory: Path,
    private val handle: SaveOutputDirectory,
    private val pins: Map<String, SaveOutputFile>,
    val manifest: JsonObject,
    val frames: List<JsonObject>,
    val recordSha256: String,
) : AutoCloseable {

    val name: String get() = directory.fileName.toString()
    private val gson = Gson()
    private val openFiles = mutableListOf<SaveOutputFile>()
    private var closed = false

    fun exists(name: String): Boolean = handle.entries().contains(name)

    fun entries(): Set<String> = handle.entries()

    /** Compare the reader itself with a still-live pin; never check one open and read another. */
    @Synchronized
    fun openFile(name: String): SaveOutputFile {
        check(!closed)
        val pin = pins[name] ?: error("保存元の実体を確認できません: $name")
        val reader = handle.openFile(name)
        try {
            check(reader.key == pin.key) { "保存元の実体が変更されています: $name" }
            openFiles.removeAll { it.isClosed }
            openFiles += reader
            return reader
        } catch (failure: Throwable) { reader.close(); throw failure }
    }

    /** The descriptor stays owned by this SavedReplay and is released on [close]. */
    fun channel(name: String): FileChannel {
        val file = openFile(name)
        try { return file.channel(setOf(READ)) }
        catch (failure: Throwable) { file.close(); throw failure }
    }

    fun text(name: String): String = channel(name).use { channel ->
        val buffer = java.nio.ByteBuffer.allocate(channel.size().toInt().also { require(it.toLong() == channel.size()) { "記録ファイルが大きすぎます: $name" } })
        while (buffer.hasRemaining()) check(channel.read(buffer) > 0)
        String(buffer.array(), Charsets.UTF_8)
    }

    fun lines(name: String): List<String> = buildList { forEachLine(name) { add(it) } }

    /** The reader and its file are closed before this call returns or throws. */
    fun forEachLine(name: String, consume: (String) -> Unit) {
        openFile(name).use { output ->
            output.channel(setOf(READ)).use { channel -> readReplayLines(channel, consume) }
        }
    }

    /** Movie-time presentation model for the player: displayed spans, gaps, touches, clock. */
    fun timeline(): ReplayTimeline {
        val spans = frames.mapNotNull { row ->
            val pts = row["movie_pts_us"]?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()
            val end = row["movie_end_us"]?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()
            if (pts != null && end != null && row["presented"]?.asBoolean == true) ReplayTimeline.Span(pts, end) else null
        }.sortedBy { it.startUs }
        val duration = manifest["parts"]?.takeIf { it.isJsonArray }?.asJsonArray?.lastOrNull()?.asJsonObject
            ?.get("movie_end_us")?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()
            ?: spans.lastOrNull()?.endUs ?: 0L
        val gaps = buildList {
            var covered = 0L
            spans.forEach { span ->
                if (span.startUs > covered) add(ReplayTimeline.Span(covered, span.startUs))
                covered = maxOf(covered, span.endUs)
            }
            if (covered < duration) add(ReplayTimeline.Span(covered, duration))
        }
        val taps = buildList {
            if (exists(ReplayFiles.TOUCHES)) forEachLine(ReplayFiles.TOUCHES) { line ->
                runCatching { gson.fromJson(line, JsonObject::class.java)["movie_us"] }
                    .getOrNull()?.takeIf { it.isJsonPrimitive }?.asString?.toLongOrNull()?.let { add(it) }
            }
        }
        val known = manifest["timebase"]?.takeIf { it.isJsonObject }?.asJsonObject?.get("known")
            ?.takeIf { it.isJsonPrimitive }?.asBoolean ?: false
        return ReplayTimeline(duration, spans, gaps, taps, known)
    }

    @Synchronized
    override fun close() {
        if (closed) return
        closed = true
        try { openFiles.forEach(SaveOutputFile::close) }
        finally {
            try { pins.values.forEach(SaveOutputFile::close) }
            finally { handle.close() }
        }
    }

    companion object {
        private val gson = Gson()

        /** Opens a completed schema-4 video save; throws IllegalArgument/IllegalState with the Japanese reason. */
        fun open(path: Path, afterHash: (String, SaveOutputFile) -> Unit = { _, _ -> }): SavedReplay {
            require(path.isAbsolute && path == path.normalize() && path.parent != null) { "保存フォルダのパスを確認してください" }
            require(!path.fileName.toString().startsWith(".") && !path.fileName.toString().endsWith(".partial")) {
                "未完成フォルダは切り取れません"
            }
            require(Files.isDirectory(path)) { "保存フォルダがありません: $path" }
            val directory = path.parent.toRealPath().resolve(path.fileName)
            val handle = SaveOutputDirectory.open(directory)
            val pins = linkedMapOf<String, SaveOutputFile>()
            fun pin(name: String) = pins.getOrPut(name) { handle.openFile(name) }
            try {
                val entries = handle.entries()
                require(ReplayFiles.RECORD in entries) { "replay.jsonのないフォルダは切り取れません" }
                val record = pin(ReplayFiles.RECORD).channel(setOf(READ)).use { channel ->
                        require(channel.size() <= 64L * 1024 * 1024) { "replay.jsonが大きすぎます" }
                        val buffer = java.nio.ByteBuffer.allocate(channel.size().toInt())
                        while (buffer.hasRemaining()) check(channel.read(buffer) > 0)
                        buffer.flip()
                        Charsets.UTF_8.decode(buffer).toString()
                }
                val manifest = try {
                    gson.fromJson(record, JsonObject::class.java)
                } catch (e: Exception) {
                    throw IllegalArgumentException("replay.jsonを読めません", e)
                }
                require(manifest.has("schema") && manifest["schema"].asInt == ReplayFiles.SCHEMA) { "保存形式が異なります（schema 4以外）" }
                require(manifest.has("complete") && manifest["complete"].asBoolean) { "未完成の保存は切り取れません" }
                val files = manifest["files"]?.takeIf { it.isJsonObject }?.asJsonObject
                    ?: throw IllegalArgumentException("replay.jsonのfilesを読めません")
                require(files["video"]?.takeIf { it.isJsonPrimitive }?.asString == ReplayFiles.VIDEO) { "動画のない保存は切り取れません" }
                require(files["video_frames"]?.takeIf { it.isJsonPrimitive }?.asString == ReplayFiles.VIDEO_FRAMES) {
                    "動画の時刻索引がない保存は切り取れません"
                }
                require(ReplayFiles.VIDEO in entries && ReplayFiles.VIDEO_FRAMES in entries) { "動画ファイルが見つかりません" }
                val hashes = manifest["files_sha256"]?.takeIf { it.isJsonObject }?.asJsonObject
                    ?: throw IllegalArgumentException("replay.jsonのfiles_sha256を読めません")
                // Select every readable input now, including legacy targets without a listed hash.
                // Those targets gain an identity pin, not a new content-validation policy.
                (hashes.keySet() + listOf(ReplayFiles.VIDEO, ReplayFiles.VIDEO_FRAMES, ReplayFiles.VIDEO_SOURCE,
                    ReplayFiles.LOGCAT_DEVICE, ReplayFiles.LOGCAT_APP, ReplayFiles.TOUCHES).filter { it in entries })
                    .forEach { pin(it) }
                hashes.entrySet().forEach { (name, expected) ->
                    val output = pins.getValue(name)
                    val actual = output.hash()
                    require(actual == expected.asString) { "保存時からファイルが変更されています: $name" }
                    afterHash(name, output)
                }
                val frames = handle.openFile(ReplayFiles.VIDEO_FRAMES).use { output ->
                    check(output.key == pins.getValue(ReplayFiles.VIDEO_FRAMES).key) { "保存元の実体が変更されています: ${ReplayFiles.VIDEO_FRAMES}" }
                    output.channel(setOf(READ)).use { channel ->
                        require(channel.size() <= Int.MAX_VALUE) { "時刻索引が大きすぎます" }
                        buildList {
                            readReplayLines(channel) { line ->
                                if (line.isNotEmpty()) add(gson.fromJson(line, JsonObject::class.java))
                            }
                        }
                    }
                }
                require(frames.isNotEmpty()) { "動画のframe記録がありません" }
                val recordHash = java.security.MessageDigest.getInstance("SHA-256").digest(record.toByteArray(Charsets.UTF_8))
                    .joinToString("") { "%02x".format(it) }
                return SavedReplay(directory, handle, pins, manifest, frames, recordHash)
            } catch (e: Throwable) {
                try { pins.values.forEach(SaveOutputFile::close) }
                finally { handle.close() }
                throw e
            }
        }
    }
}

internal fun readReplayLines(channel: ReadableByteChannel, consume: (String) -> Unit) {
    Channels.newInputStream(channel).bufferedReader(Charsets.UTF_8).use { reader ->
        reader.lineSequence().forEach(consume)
    }
}

package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonObject
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ

/** A completed schema-4 saved replay folder opened read-only through the save handles. */
internal class SavedReplay private constructor(
    val directory: Path,
    private val handle: SaveOutputDirectory,
    val manifest: JsonObject,
    val frames: List<JsonObject>,
) : AutoCloseable {

    val name: String get() = directory.fileName.toString()
    private val openFiles = mutableListOf<SaveOutputFile>()

    fun exists(name: String): Boolean = handle.entries().contains(name)

    fun entries(): Set<String> = handle.entries()

    fun openFile(name: String): SaveOutputFile = handle.openFile(name)

    /** The descriptor stays owned by this SavedReplay and is released on [close]. */
    fun channel(name: String): FileChannel {
        val file = handle.openFile(name)
        openFiles += file
        return file.channel(setOf(READ))
    }

    fun text(name: String): String = channel(name).use { channel ->
        val buffer = java.nio.ByteBuffer.allocate(channel.size().toInt().also { require(it.toLong() == channel.size()) { "記録ファイルが大きすぎます: $name" } })
        while (buffer.hasRemaining()) check(channel.read(buffer) > 0)
        String(buffer.array(), Charsets.UTF_8)
    }

    fun lines(name: String): List<String> = text(name).let { text ->
        if (text.isEmpty()) emptyList() else text.removeSuffix("\n").split('\n')
    }

    override fun close() {
        openFiles.forEach(SaveOutputFile::close)
        handle.close()
    }

    companion object {
        private val gson = Gson()

        /** Opens a completed schema-4 video save; throws IllegalArgument/IllegalState with the Japanese reason. */
        fun open(path: Path): SavedReplay {
            require(path.isAbsolute && path == path.normalize()) { "保存フォルダのパスを確認してください" }
            require(!path.fileName.toString().startsWith(".") && !path.fileName.toString().endsWith(".partial")) {
                "未完成フォルダは切り取れません"
            }
            require(Files.isDirectory(path)) { "保存フォルダがありません: $path" }
            val handle = SaveOutputDirectory.open(path)
            try {
                val entries = handle.entries()
                require(ReplayFiles.RECORD in entries) { "replay.jsonのないフォルダは切り取れません" }
                val record = handle.openFile(ReplayFiles.RECORD).use { output ->
                    output.channel(setOf(READ)).use { channel ->
                        require(channel.size() <= 64L * 1024 * 1024) { "replay.jsonが大きすぎます" }
                        val buffer = java.nio.ByteBuffer.allocate(channel.size().toInt())
                        while (buffer.hasRemaining()) check(channel.read(buffer) > 0)
                        buffer.flip()
                        Charsets.UTF_8.decode(buffer).toString()
                    }
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
                hashes.entrySet().forEach { (name, expected) ->
                    val actual = handle.openFile(name).use { it.hash() }
                    require(actual == expected.asString) { "保存時からファイルが変更されています: $name" }
                }
                val frames = handle.openFile(ReplayFiles.VIDEO_FRAMES).use { output ->
                    output.channel(setOf(READ)).use { channel ->
                        val buffer = java.nio.ByteBuffer.allocate(channel.size().toInt().also {
                            require(it.toLong() == channel.size()) { "時刻索引が大きすぎます" }
                        })
                        while (buffer.hasRemaining()) check(channel.read(buffer) > 0)
                        String(buffer.array(), Charsets.UTF_8).removeSuffix("\n").split('\n')
                            .filter(String::isNotEmpty)
                            .map { gson.fromJson(it, JsonObject::class.java) }
                    }
                }
                require(frames.isNotEmpty()) { "動画のframe記録がありません" }
                return SavedReplay(path, handle, manifest, frames)
            } catch (e: Throwable) {
                handle.close()
                throw e
            }
        }
    }
}

package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonParser
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermissions

/** Immutable identities of a capture whose host readers/clients have already stopped. */
internal data class RemoteCleanup(val serial: String, val token: String, val forwards: Map<String, String>) {
    private val serverName get() = "replay-$token-video"
    private val clockName get() = "replay-$token-clock"

    fun validate(): RemoteCleanup {
        serialArgument(serial)
        require(Regex("[0-9a-f]{32}").matches(token) && forwards.size <= 64)
        forwards.forEach { (port, endpoint) ->
            require(Regex("[1-9][0-9]{0,4}").matches(port) && port.toInt() <= 65535)
            require(Regex("localabstract:scrcpy_[0-9a-f]{8}").matches(endpoint))
        }
        return this
    }

    fun clean(adbPath: Path, cancelled: () -> Boolean = { false }): Boolean = runCatching {
        validate()
        OwnedAdb(adbPath).use { adb ->
            fun command(vararg args: String): String { check(!cancelled()); return adb.command(*args) }
            fun pids(): Set<Int> = command("-s", serial, "shell", "ps", "-A", "-o", "PID,ARGS")
                .lineSequence().drop(1).mapNotNull { line ->
                    val fields = line.trim().split(Regex("\\s+"))
                    if (fields.size >= 2 && fields[1] in setOf(serverName, clockName)) fields[0].toIntOrNull()?.takeIf { it > 1 } else null
                }.toSet()
            pids().forEach { pid -> if (pid in pids()) command("-s", serial, "shell", "kill", "-TERM", pid.toString()) }
            check(pids().isEmpty()) { "所有端末processの終了が未確認です" }
            removeOwnedForwards(adb, serial, forwards, cancelled)
            command("-s", serial, "shell", "rm", "-f", "/data/local/tmp/replay-$token-server.jar", "/data/local/tmp/replay-$token-clock.jar")
        }
        true
    }.getOrDefault(false)
}

/** A recycled TCP port is no longer ours. Never remove a mapping by its port alone. */
internal fun removeOwnedForwards(adb: OwnedAdb, serial: String, forwards: Map<String, String>, cancelled: () -> Boolean = { false }) {
    forwards.forEach { (port, endpoint) ->
        check(!cancelled())
        val listed = adb.command("forward", "--list").lineSequence().filter { it.isNotBlank() }.map { it.trim().split(Regex("\\s+")) }.toList()
        check(listed.all { it.size == 3 }) { "forwardの所有を確認できません" }
        val matches = listed.filter { it[1] == "tcp:$port" }
        check(!cancelled())
        if (matches.singleOrNull() == listOf(serial, "tcp:$port", endpoint))
            adb.command("-s", serial, "forward", "--remove", "tcp:$port")
        else check(matches.size <= 1) { "forwardの所有を確認できません" }
    }
}

/** Only this caller-supplied private directory is inspected; no workspace/host-wide search or sweep. */
internal class RemoteCleanupJournal(private var directory: Path) : AutoCloseable {
    private data class Entry(val record: RemoteCleanup, val channel: FileChannel, val lock: FileLock, val fileKey: Any)
    private val owned = linkedMapOf<Path, Entry>()
    private var other = 0
    val pendingCount get() = owned.size + other
    val hasRecoverable get() = owned.isNotEmpty()

    init { require(directory.isAbsolute) }

    fun retain(record: RemoteCleanup) {
        record.validate()
        prepare()
        val path = directory.resolve("remote-cleanup-${record.token}.json")
        if (path in owned) return
        val bytes = Gson().toJson(mapOf("schema" to 1, "serial" to record.serial, "token" to record.token, "forwards" to record.forwards)).toByteArray(Charsets.UTF_8)
        val channel = FileChannel.open(path, setOf<java.nio.file.OpenOption>(CREATE_NEW, READ, WRITE, NOFOLLOW_LINKS),
            PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
        try {
            val key = key(path)
            val lock = channel.tryLock() ?: error("終了情報の所有を確認できません")
            val buffer = ByteBuffer.wrap(bytes)
            while (buffer.hasRemaining()) channel.write(buffer)
            channel.force(true)
            owned[path] = Entry(record, channel, lock, key)
        } catch (e: Exception) {
            channel.close()
            // An incomplete new record is preserved for diagnosis, never treated as safe remote ownership.
            throw e
        }
    }

    fun recover() {
        other = 0
        if (!Files.exists(directory, NOFOLLOW_LINKS)) return
        prepare()
        Files.newDirectoryStream(directory, "remote-cleanup-*.json").use { paths ->
            var count = 0
            paths.forEach { path ->
                check(++count <= 256) { "終了情報の上限を超えました。所有情報を保全しています" }
                if (path in owned) return@forEach
                if (!Regex("remote-cleanup-[0-9a-f]{32}\\.json").matches(path.fileName.toString())) { other++; return@forEach }
                var channel: FileChannel? = null
                try {
                    require(Files.isRegularFile(path, NOFOLLOW_LINKS))
                    val key = key(path)
                    channel = FileChannel.open(path, READ, WRITE, NOFOLLOW_LINKS)
                    val lock = channel.tryLock() ?: error("別ownerが終了情報を確認中です")
                    require(key(path) == key && channel.size() in 1..65536)
                    val buffer = ByteBuffer.allocate(channel.size().toInt())
                    while (buffer.hasRemaining()) check(channel.read(buffer) > 0)
                    val json = JsonParser.parseString(buffer.array().toString(Charsets.UTF_8)).asJsonObject
                    require(json.keySet() == setOf("schema", "serial", "token", "forwards") && json["schema"].toString() == "1")
                    val forwards = json["forwards"].asJsonObject.entrySet().associate { it.key to it.value.asString }
                    val record = RemoteCleanup(json["serial"].asString, json["token"].asString, java.util.Map.copyOf(forwards)).validate()
                    require(path.fileName.toString() == "remote-cleanup-${record.token}.json")
                    owned[path] = Entry(record, channel, lock, key)
                    channel = null // ownership transfers only after validation.
                } catch (_: Exception) { other++ } finally { channel?.close() }
            }
        }
    }

    fun clean(adbPath: Path, connectedSerials: Set<String>, cancelled: () -> Boolean = { false }) {
        owned.toMap().forEach { (path, entry) ->
            if (!cancelled() && entry.record.serial in connectedSerials && entry.record.clean(adbPath, cancelled)) {
                check(key(path) == entry.fileKey) { "終了情報の所有が変更されています" }
                Files.delete(path)
                entry.lock.release(); entry.channel.close(); owned.remove(path)
            }
        }
    }

    private fun prepare() {
        require(!Files.isSymbolicLink(directory)) { "終了情報の保存先を確認してください" }
        Files.createDirectories(directory)
        directory = directory.toRealPath()
        Files.setPosixFilePermissions(directory, PosixFilePermissions.fromString("rwx------"))
    }

    private fun key(path: Path): Any = Files.readAttributes(path, BasicFileAttributes::class.java, NOFOLLOW_LINKS).fileKey()
        ?: error("終了情報のfile identityを確認できません")

    override fun close() {
        owned.values.forEach { it.lock.release(); it.channel.close() }
        other += owned.size
        owned.clear()
    }
}

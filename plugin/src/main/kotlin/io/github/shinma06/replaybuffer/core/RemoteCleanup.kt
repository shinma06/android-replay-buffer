package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonParser
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.NoSuchFileException
import java.nio.file.Path
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFilePermissions

/** Immutable identities retained while stopping a capture, including unconfirmed host termination. */
internal data class RemoteCleanup(
    val serial: String,
    val token: String,
    val forwards: Map<String, String>,
    val pendingEndpoints: Set<String> = emptySet(),
) {
    private val serverName get() = "replay-$token-video"
    private val clockName get() = "replay-$token-clock"

    fun validate(): RemoteCleanup {
        serialArgument(serial)
        require(Regex("[0-9a-f]{32}").matches(token) && forwards.size + pendingEndpoints.size <= 64)
        pendingEndpoints.forEach { require(Regex("localabstract:scrcpy_[0-9a-f]{8}").matches(it)) }
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
            removeOwnedForwards(adb, serial, forwards, cancelled, pendingEndpoints)
            command("-s", serial, "shell", "rm", "-f", "/data/local/tmp/replay-$token-server.jar", "/data/local/tmp/replay-$token-clock.jar")
        }
        true
    }.getOrDefault(false)
}

internal fun listForwards(adb: OwnedAdb, cancelled: () -> Boolean = { false }): List<List<String>> {
    check(!cancelled())
    val result = adb.command("forward", "--list").lineSequence().filter { it.isNotBlank() }
        .map { it.trim().split(Regex("\\s+")) }.toList()
    check(result.all { it.size == 3 }) { "forwardの所有を確認できません" }
    return result
}

/** A recycled TCP port is no longer ours. Never remove a mapping by its port alone. */
internal fun removeOwnedForwards(
    adb: OwnedAdb,
    serial: String,
    forwards: Map<String, String>,
    cancelled: () -> Boolean = { false },
    pendingEndpoints: Set<String> = emptySet(),
) {
    forwards.forEach { (port, endpoint) ->
        val matches = listForwards(adb, cancelled).filter { it[1] == "tcp:$port" }
        check(!cancelled())
        if (matches.singleOrNull() == listOf(serial, "tcp:$port", endpoint))
            adb.command("-s", serial, "forward", "--remove", "tcp:$port")
        else check(matches.size <= 1) { "forwardの所有を確認できません" }
    }
    pendingEndpoints.forEach { endpoint ->
        val matches = listForwards(adb, cancelled).filter { it[0] == serial && it[2] == endpoint }
        check(matches.size <= 1) { "未確定forwardの所有を確認できません" }
        matches.singleOrNull()?.let { identity ->
            val port = identity[1].removePrefix("tcp:")
            require(identity[1] == "tcp:$port" && Regex("[1-9][0-9]{0,4}").matches(port) && port.toInt() <= 65535)
            removeOwnedForwards(adb, serial, mapOf(port to endpoint), cancelled)
        }
        check(listForwards(adb, cancelled).none { it[0] == serial && it[2] == endpoint }) { "未確定forwardの削除を確認できません" }
    }
}

/**
 * The core serializes this journal's calls in its caller-supplied private directory; no host-wide sweep.
 * Recovery reads/locks descriptors without contacting devices; pending count also includes quarantined
 * records. Close releases locks and keeps descriptors. Retire removes only a verified owned record
 * after the caller confirms the corresponding client/reader and remote cleanup outcome.
 */
internal class RemoteCleanupJournal(
    private var directory: Path,
    private val createChannel: (Path) -> FileChannel,
) : AutoCloseable {
    constructor(directory: Path) : this(directory, { path ->
        FileChannel.open(path, setOf<java.nio.file.OpenOption>(CREATE_NEW, READ, WRITE, NOFOLLOW_LINKS),
            PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
    })
    private data class Entry(val record: RemoteCleanup, val channel: FileChannel, val lock: FileLock, val fileKey: Any, val hostPending: Boolean = false)
    private val owned = linkedMapOf<Path, Entry>()
    private var other = 0
    val pendingCount get() = owned.size + other
    val hasRecoverable get() = owned.values.any { !it.hostPending }

    init { require(directory.isAbsolute) }

    fun retain(record: RemoteCleanup, hostPending: Boolean = false) {
        record.validate()
        prepare()
        val path = directory.resolve("remote-cleanup-${record.token}.json")
        val fields = mutableMapOf<String, Any>("schema" to 1, "serial" to record.serial, "token" to record.token, "forwards" to record.forwards)
        if (record.pendingEndpoints.isNotEmpty()) {
            fields["schema"] = 2
            fields["pending_endpoints"] = record.pendingEndpoints
        }
        if (hostPending) {
            fields["schema"] = 3
            fields["pending_endpoints"] = record.pendingEndpoints
            fields["host_pending"] = true
        }
        val bytes = Gson().toJson(fields).toByteArray(Charsets.UTF_8)
        val previous = owned[path]
        if (previous != null) check(previous.record.token == record.token && previous.record.serial == record.serial &&
            previous.lock.isValid && key(path) == previous.fileKey) { "終了情報の所有を確認できません" }
        val staged = directory.resolve(".remote-cleanup-${record.token}-${java.util.UUID.randomUUID()}.tmp")
        val channel = createChannel(staged)
        var candidate: Entry? = null
        var stagedKey: Any? = null
        var published = false
        try {
            stagedKey = key(staged)
            val lock = channel.tryLock() ?: error("終了情報の所有を確認できません")
            candidate = Entry(record, channel, lock, stagedKey, hostPending)
            val buffer = ByteBuffer.wrap(bytes)
            while (buffer.hasRemaining()) channel.write(buffer)
            channel.force(true)
            check(lock.isValid && key(staged) == stagedKey) { "終了情報の所有が変更されています" }
            if (previous != null) {
                check(previous.lock.isValid && key(path) == previous.fileKey) { "終了情報の所有が変更されています" }
                // Both old and replacement inodes remain locked across atomic publication.
                Files.move(staged, path, ATOMIC_MOVE, REPLACE_EXISTING)
            } else {
                // Hard-link publication is exclusive: never replace a foreign same-token descriptor.
                Files.createLink(path, staged)
            }
            owned[path] = candidate
            published = true
            if (previous == null) unlinkOwnedStage(staged, stagedKey)
            previous?.let(::release)
        } catch (failure: Throwable) {
            if (!published) {
                runCatching { candidate?.let(::release) ?: channel.close() }.exceptionOrNull()?.let(failure::addSuppressed)
                runCatching { unlinkOwnedStage(staged, stagedKey) }.exceptionOrNull()?.let(failure::addSuppressed)
            }
            throw failure
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
                    val keys = setOf("schema", "serial", "token", "forwards")
                    val endpoints = when (json["schema"].toString()) {
                        "1" -> { require(json.keySet() == keys); emptyList() }
                        "2", "3" -> {
                            val hostPending = json["schema"].toString() == "3"
                            require(json.keySet() == keys + "pending_endpoints" + if (hostPending) setOf("host_pending") else emptySet())
                            if (hostPending) require(json["host_pending"].toString() == "true")
                            json["pending_endpoints"].asJsonArray.map { it.asString }
                        }
                        else -> error("終了情報のschemaを確認できません")
                    }
                    require(endpoints.size == endpoints.toSet().size)
                    val forwards = json["forwards"].asJsonObject.entrySet().associate { it.key to it.value.asString }
                    val record = RemoteCleanup(json["serial"].asString, json["token"].asString,
                        java.util.Map.copyOf(forwards), java.util.Set.copyOf(endpoints)).validate()
                    require(path.fileName.toString() == "remote-cleanup-${record.token}.json")
                    // shortcut: Disposed unresolved owners stay quarantined; automatic recovery needs a verified completion receipt.
                    if (json["schema"].toString() == "3") { other++; return@forEach }
                    owned[path] = Entry(record, channel, lock, key)
                    channel = null // ownership transfers only after validation.
                } catch (_: Exception) { other++ } finally { channel?.close() }
            }
        }
    }

    fun clean(adbPath: Path, connectedSerials: Set<String>, cancelled: () -> Boolean = { false }) {
        owned.toMap().forEach { (path, entry) ->
            if (!entry.hostPending && !cancelled() && entry.record.serial in connectedSerials) {
                check(entry.lock.isValid && key(path) == entry.fileKey) { "終了情報の所有が変更されています" }
                if (entry.record.clean(adbPath, cancelled)) retire(entry.record)
            }
        }
    }

    fun retire(record: RemoteCleanup) {
        record.validate()
        val path = directory.resolve("remote-cleanup-${record.token}.json")
        val entry = owned[path] ?: return
        check(entry.record.token == record.token && entry.record.serial == record.serial && entry.lock.isValid && key(path) == entry.fileKey) {
            "終了情報の所有が変更されています"
        }
        Files.delete(path)
        owned.remove(path) // Descriptor deletion commits retirement, even if handle cleanup fails.
        release(entry)
    }

    private fun unlinkOwnedStage(path: Path, fileKey: Any?) {
        if (fileKey == null) return
        val currentKey = try { key(path) } catch (_: NoSuchFileException) { return }
        check(currentKey == fileKey) { "終了情報の所有が変更されています" }
        // shortcut: Path checks do not exclude later same-user replacement; use descriptor-relative unlink when supported.
        Files.delete(path)
    }

    private fun release(entry: Entry) {
        val lockFailure = runCatching { entry.lock.release() }.exceptionOrNull()
        val channelFailure = runCatching { entry.channel.close() }.exceptionOrNull()
        if (lockFailure != null) {
            channelFailure?.let(lockFailure::addSuppressed)
            throw lockFailure
        }
        channelFailure?.let { throw it }
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
        val entries = owned.values.toList()
        other += owned.size
        owned.clear()
        var failure: Throwable? = null
        entries.forEach { entry ->
            runCatching { release(entry) }.exceptionOrNull()?.let { error ->
                if (failure == null) failure = error else failure.addSuppressed(error)
            }
        }
        failure?.let { throw it }
    }
}

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
import java.nio.file.attribute.PosixFileAttributes
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions
import java.time.Instant
import java.util.ArrayDeque
import java.util.UUID
import java.util.zip.CRC32

/** A locked record for one local capture, never a saved output or a host-temp search. */
internal class LocalCaptureOwnership private constructor(
    val root: Path,
    private val descriptor: Path,
    private val channel: FileChannel,
    private val lock: FileLock,
) : AutoCloseable {
    private val descriptorIdentity = identity(descriptor)
    private val namespaceIdentity = identity(root.parent)
    private val process = ProcessHandle.current()
    private val started = process.info().startInstant().orElseThrow()
    private data class Entry(val slot: Int, val identity: List<String>)
    private val entries = mutableMapOf<String, Entry>()
    private val freeSlots = ArrayDeque<Int>()
    private var slots = 0
    private var failed = false

    init {
        persist(0, encodeRecord(mapOf("schema" to 2, "token" to root.fileName.toString().removePrefix("replay-"),
            "pid" to process.pid(), "started" to started.toString(), "namespace" to namespaceIdentity,
            "descriptor" to descriptorIdentity), HEADER_BYTES))
    }

    @Synchronized
    fun created(path: Path) {
        check(!failed) { "local所有記録の書込みが不確かです（保全しました）" }
        require(identity(root.parent) == namespaceIdentity && identity(descriptor) == descriptorIdentity)
        val name = if (path == root) "." else root.relativize(path).toString()
        require(allowed(name) && name !in entries && entries.size < MAX_ENTRIES)
        val identity = identity(path)
        val slot = freeSlots.peekFirst() ?: slots.also { require(it < MAX_ENTRIES) }
        persist(HEADER_BYTES.toLong() + slot.toLong() * SLOT_BYTES, encodeRecord(listOf(name, identity), SLOT_BYTES))
        if (freeSlots.isEmpty()) slots++ else freeSlots.removeFirst()
        entries[name] = Entry(slot, identity)
    }

    @Synchronized
    fun removed(path: Path) {
        require(identity(root.parent) == namespaceIdentity && identity(descriptor) == descriptorIdentity)
        val name = root.relativize(path).toString()
        val entry = entries[name] ?: error("local削除対象の所有記録がありません")
        val absent = try { identity(path); false } catch (_: java.nio.file.NoSuchFileException) { true }
        require(absent)
        entries.remove(name)
        // Missing old UUID names are safe on recovery; durable replacement precedes the next payload.
        freeSlots.addLast(entry.slot)
    }

    private fun persist(offset: Long, bytes: ByteArray) {
        try {
            val expectedSize = if (offset == 0L) 0L else HEADER_BYTES.toLong() + slots.toLong() * SLOT_BYTES
            require(channel.size() == expectedSize)
            channel.position(offset)
            val buffer = ByteBuffer.wrap(bytes)
            while (buffer.hasRemaining()) check(channel.write(buffer) > 0)
            channel.force(true)
        } catch (failure: Exception) {
            failed = true // Do not write around an incomplete slot; preserve ambiguity until normal cleanup.
            throw failure
        }
    }

    fun removeRoot() {
        val current = try { identity(root) } catch (_: java.nio.file.NoSuchFileException) { return }
        require(identity(root.parent) == namespaceIdentity && current == entries["."]?.identity)
        Files.delete(root) // Unknown/nonempty roots stay intact; only exact absence is idempotent.
    }

    @Synchronized
    override fun close() {
        try {
            val rootAbsent = try { identity(root); false } catch (_: java.nio.file.NoSuchFileException) { true }
            val currentDescriptor = try { identity(descriptor) } catch (_: java.nio.file.NoSuchFileException) { return }
            if (rootAbsent) {
                require(identity(root.parent) == namespaceIdentity && currentDescriptor == descriptorIdentity) {
                    "local終了情報のidentityが不明です（保全しました）"
                }
                Files.delete(descriptor)
            }
        } finally {
            try { if (lock.isValid) lock.release() } finally { channel.close() }
        }
    }

    companion object {
        // One live packet ring + one pinned ring + root/ring/resources + a not-yet-indexed GOP.
        internal const val MAX_ENTRIES = 2 * ReplaySettings.MAX_VIDEO_PACKETS + 5
        private const val MAX_IDENTITY_BYTES = 1024
        internal const val HEADER_BYTES = 8192
        internal const val SLOT_BYTES = MAX_IDENTITY_BYTES + 64
        internal const val MAX_BYTES = (MAX_ENTRIES + 2) * (MAX_IDENTITY_BYTES + 64) + 8192
        private val tokenPattern = Regex("[0-9a-f-]{36}")
        private val packetPattern = Regex("ring/gop-[0-9a-f-]{36}\\.h264")
        private fun allowed(name: String) = name in setOf(".", "ring", "scrcpy-server-v4.0", "scrcpy-server-v5.0.1", "replay-clock.jar") || packetPattern.matches(name)
        private fun identity(attributes: PosixFileAttributes): List<String> {
            require(!attributes.isSymbolicLink && (attributes.isDirectory || attributes.isRegularFile))
            require(attributes.owner().name == System.getProperty("user.name") && attributes.permissions().none {
                it == PosixFilePermission.GROUP_WRITE || it == PosixFilePermission.OTHERS_WRITE
            })
            return listOf(attributes.fileKey()?.toString() ?: error("local file identityが不明です"),
                attributes.creationTime().toString(), attributes.owner().name, attributes.permissions().sorted().joinToString(",")).also {
                require(Gson().toJson(it).toByteArray(Charsets.UTF_8).size <= MAX_IDENTITY_BYTES)
            }
        }
        private fun identity(path: Path) = identity(Files.readAttributes(path, PosixFileAttributes::class.java, NOFOLLOW_LINKS))

        fun create(namespace: Path): LocalCaptureOwnership {
            require(namespace.isAbsolute && !Files.isSymbolicLink(namespace))
            val token = UUID.randomUUID().toString()
            val root = namespace.resolve("replay-$token")
            val descriptor = namespace.resolve("local-capture-$token.json")
            val channel = FileChannel.open(descriptor, setOf<java.nio.file.OpenOption>(CREATE_NEW, READ, WRITE, NOFOLLOW_LINKS),
                PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
            try {
                val lock = channel.tryLock() ?: error("local ownerを取得できません")
                Files.createDirectory(root, PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
                val ownership = LocalCaptureOwnership(root, descriptor, channel, lock)
                ownership.created(root)
                return ownership
            } catch (failure: Exception) {
                // Incomplete creation is ambiguous: preserve it, release the live handle only.
                channel.close()
                throw failure
            }
        }

        fun recover(namespace: Path) {
            require(namespace.isAbsolute && !Files.isSymbolicLink(namespace))
            val namespaceIdentity = try { identity(namespace) } catch (_: java.nio.file.NoSuchFileException) { return }
            Files.newDirectoryStream(namespace).use { stream ->
                var count = 0
                var visited = 0
                for (path in stream) {
                    check(++visited <= 1024) { "local namespaceの上限を超えました（保全しました）" }
                    val name = path.fileName
                    if (!name.toString().startsWith("local-capture-")) continue
                    check(++count <= 256) { "local終了情報の上限を超えました（保全しました）" }
                    runCatching {
                        val token = name.toString().removePrefix("local-capture-").removeSuffix(".json")
                        require(tokenPattern.matches(token) && UUID.fromString(token).toString() == token && name.toString() == "local-capture-$token.json")
                        val recordIdentity = identity(path)
                        FileChannel.open(path, READ, WRITE, NOFOLLOW_LINKS).use { channel ->
                            channel.tryLock()?.use {
                                val size = channel.size()
                                require(identity(path) == recordIdentity && size in (HEADER_BYTES + SLOT_BYTES)..MAX_BYTES.toLong())
                                require((size - HEADER_BYTES) % SLOT_BYTES == 0L)
                                val slotCount = (size - HEADER_BYTES) / SLOT_BYTES
                                require(slotCount in 1..MAX_ENTRIES.toLong())
                                // Schema1 whole-JSON records cannot pass this framing; conservatively retain them.
                                val json = readRecord(channel, HEADER_BYTES).asJsonObject
                                require(json.keySet() == setOf("schema", "token", "pid", "started", "namespace", "descriptor"))
                                require(json["schema"].toString() == "2" && json["token"].asString == token)
                                val pid = json["pid"].asLong
                                val started = Instant.parse(json["started"].asString)
                                require(pid > 0 && json["pid"].toString() == pid.toString() && started <= Instant.now())
                                val current = ProcessHandle.of(pid).orElse(null)
                                require(ownerStopped(started, current != null, current?.info()?.startInstant()?.orElse(null)))
                                require(json["namespace"].asJsonArray.map { it.asString } == namespaceIdentity)
                                require(json["descriptor"].asJsonArray.map { it.asString } == recordIdentity)
                                val entries = mutableMapOf<String, List<String>>()
                                repeat(slotCount.toInt()) {
                                    val entry = readRecord(channel, SLOT_BYTES).asJsonArray
                                    require(entry.size() == 2)
                                    val name = entry[0].asString
                                    require(allowed(name))
                                    val identity = entry[1].asJsonArray.map { it.asString }
                                    require(identity.size == 4 && Gson().toJson(identity).toByteArray(Charsets.UTF_8).size <= MAX_IDENTITY_BYTES)
                                    require(entries.put(name, identity) == null)
                                }
                                require("." in entries)
                                require(identity(namespace) == namespaceIdentity)
                                reclaim(namespace, namespaceIdentity, namespace.resolve("replay-$token"), entries)
                                require(identity(namespace) == namespaceIdentity && identity(path) == recordIdentity)
                                Files.delete(path)
                            }
                        }
                    } // Unknown, live, replaced, or partially written records stay intact.
                }
            }
        }

        internal fun encodeRecord(value: Any, size: Int): ByteArray {
            val bytes = Gson().toJson(value).toByteArray(Charsets.UTF_8)
            require(bytes.size in 1..(size - 8))
            return ByteBuffer.allocate(size).putInt(bytes.size).putInt(CRC32().apply { update(bytes) }.value.toInt())
                .put(bytes).array()
        }

        internal fun decodeRecord(record: ByteArray): com.google.gson.JsonElement {
            val buffer = ByteBuffer.wrap(record)
            val size = buffer.int
            val checksum = buffer.int
            require(size in 1..(record.size - 8))
            val bytes = ByteArray(size).also { buffer.get(it) }
            require(CRC32().apply { update(bytes) }.value.toInt() == checksum)
            while (buffer.hasRemaining()) require(buffer.get() == 0.toByte())
            val json = JsonParser.parseString(bytes.toString(Charsets.UTF_8))
            require(Gson().toJson(json).toByteArray(Charsets.UTF_8).contentEquals(bytes))
            return json
        }

        private fun readRecord(channel: FileChannel, size: Int): com.google.gson.JsonElement {
            val buffer = ByteBuffer.allocate(size)
            while (buffer.hasRemaining()) check(channel.read(buffer) > 0)
            return decodeRecord(buffer.array())
        }

        internal fun ownerStopped(recorded: Instant, present: Boolean, current: Instant?): Boolean =
            !present || current != null && current != recorded

        private fun reclaim(namespace: Path, namespaceIdentity: List<String>, root: Path, entries: Map<String, List<String>>) {
            val rootIdentity = try { identity(root) } catch (_: java.nio.file.NoSuchFileException) { return }
            require(Files.isDirectory(root, NOFOLLOW_LINKS) && rootIdentity == entries["."])
            val children = Files.newDirectoryStream(root).use { it.take(4) }.also { require(it.size <= 3) }
            val ring = root.resolve("ring")
            val packets = mutableListOf<Path>()
            for (path in children) {
                require(identity(path) == entries[path.fileName.toString()])
                if (path == ring) {
                    require(Files.isDirectory(path, NOFOLLOW_LINKS))
                    packets += Files.newDirectoryStream(path).use { it.take(MAX_ENTRIES + 1) }.also { require(it.size <= MAX_ENTRIES) }
                } else require(path.fileName.toString() in setOf("scrcpy-server-v4.0", "scrcpy-server-v5.0.1", "replay-clock.jar") && Files.isRegularFile(path, NOFOLLOW_LINKS))
            }
            for (path in packets) {
                require(Files.isRegularFile(path, NOFOLLOW_LINKS) && identity(path) == entries["ring/${path.fileName}"])
            }
            // Validate the entire bounded tree before deletion, then recheck every parent and target.
            // shortcut: path checks cannot make unlink atomic with same-user replacement; use descriptor-relative deletion when the IDE provider supports it.
            for (path in packets + children.sortedBy { it == ring } + listOf(root)) {
                require(identity(namespace) == namespaceIdentity && identity(root) == entries["."])
                if (path.parent == ring) require(identity(ring) == entries["ring"])
                val name = if (path == root) "." else root.relativize(path).toString()
                require(identity(path) == entries[name])
                Files.delete(path)
            }
        }
    }
}

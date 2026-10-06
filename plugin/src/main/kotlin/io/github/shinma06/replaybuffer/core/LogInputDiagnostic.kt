package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.io.FilterInputStream
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.PosixFilePermissions

/** Opt-in evidence from the actual reader, never another logcat connection. All I/O follows reader shutdown. */
internal class LogInputDiagnostic private constructor(
    private val root: Path?,
    rawLimit: Int,
    private val metadataLimit: Int,
    private val readLimit: Int,
    private val now: () -> Long,
    private val persist: (Path, ByteArray, Int) -> Unit,
) {
    private val directoryKey = root?.let { Files.readAttributes(it, java.nio.file.attribute.BasicFileAttributes::class.java, NOFOLLOW_LINKS).fileKey() }
    private val raw = ByteArray(rawLimit)
    private var rawSize = 0
    private val metadata = ByteArrayOutputStream(metadataLimit)
    private val output = DataOutputStream(metadata)
    private val started = now()
    private var offset = 0L
    private var reads = 0
    private var connections = 0
    private var reason: String? = if (root == null) "configuration" else null
    private var sealed = false

    @Synchronized
    fun position(): Long = offset

    @Synchronized
    fun input(source: InputStream, stopping: () -> Boolean = { false }): InputStream {
        check(!sealed)
        connections++
        if (connections != 1) fail("multiple_connections")
        if (reason != null) return source
        return object : FilterInputStream(source) {
            override fun read(): Int {
                val before = now()
                val value = try { source.read() } catch (error: Exception) { if (!stopping()) failedRead(); throw error }
                val after = now()
                received(if (value < 0) byteArrayOf() else byteArrayOf(value.toByte()), 0,
                    if (value < 0) -1 else 1, before, after)
                return value
            }

            override fun read(bytes: ByteArray, start: Int, length: Int): Int {
                val before = now()
                val count = try { source.read(bytes, start, length) } catch (error: Exception) { if (!stopping()) failedRead(); throw error }
                received(bytes, start, count, before, now())
                return count
            }
        }
    }

    @Synchronized
    private fun received(bytes: ByteArray, start: Int, count: Int, before: Long, after: Long) {
        val begin = offset
        if (count > 0) offset += count
        if (reason != null) return
        if (++reads > readLimit) { fail("read_limit"); return }
        if (after - started > MAX_DURATION_NS) { fail("duration_limit"); return }
        if (count > raw.size - rawSize) { fail("raw_limit"); return }
        if (count > 0) { bytes.copyInto(raw, rawSize, start, start + count); rawSize += count }
        event(29, 'R') { writeLong(begin); writeInt(count); writeLong(before); writeLong(after) }
    }

    @Synchronized
    private fun failedRead() { fail("read_exception") }

    @Synchronized
    fun parsed(begin: Long, live: Boolean) = event(26, 'P') {
        writeLong(begin); writeLong(offset); writeLong(now()); writeBoolean(live)
    }

    @Synchronized
    fun parseFailed() { fail("parse_exception") }

    @Synchronized
    fun stored(begin: Long, entered: Long, id: String?) {
        if (id != null && !validId(id)) { fail("invalid_identity"); return }
        event(if (id == null) 26 else 64, 'S') {
            writeLong(begin); writeLong(entered); writeLong(now()); writeBoolean(id != null)
            if (id != null) writeUTF(id)
        }
    }

    @Synchronized
    fun saveStarted() = event(9, 'B') { writeLong(now()) }

    @Synchronized
    fun frozen(capture: FrozenCapture?) {
        if (capture == null) { event(9, 'N') { writeLong(now()) }; return }
        if (!validId(capture.id) || !validId(capture.sequence) || capture.logs.any { !validId(it.id) }) {
            fail("invalid_identity"); return
        }
        val required = 113L + capture.logs.size * 38L
        if (required > metadataLimit) { fail("metadata_limit"); return }
        event(required.toInt(), 'F') {
            writeLong(now()); writeUTF(capture.id); writeUTF(capture.sequence); writeLong(capture.start); writeLong(capture.end)
            writeLong(capture.generation); writeInt(capture.logs.size)
            capture.logs.forEach { writeUTF(it.id) }
        }
    }

    /** Caller proves all capture workers stopped first. A failed dump never replaces product shutdown. */
    @Synchronized
    fun finish(generation: Long, serial: String): Boolean {
        if (sealed) return false
        sealed = true
        val directory = root ?: return false
        return try {
            fun save(name: String, bytes: ByteArray, count: Int) {
                validateRoot(directory)
                require(directoryKey != null && directoryKey == Files.readAttributes(directory,
                    java.nio.file.attribute.BasicFileAttributes::class.java, NOFOLLOW_LINKS).fileKey())
                persist(directory.resolve(name), bytes, count)
            }
            save("input.bin.partial", raw, rawSize)
            val receipt = metadata.toByteArray()
            save("receipt.bin.partial", receipt, receipt.size)
            val description = Gson().toJson(mapOf("schema" to 1, "build" to CaptureResources.identity(),
                "generation" to generation, "serial" to serial, "connections" to connections,
                "raw_bytes" to rawSize, "observed_bytes" to offset, "metadata_bytes" to receipt.size,
                "reads" to reads, "complete" to (reason == null), "reason" to reason,
                "raw_sha256" to sha256(directory.resolve("input.bin.partial")),
                "receipt_sha256" to sha256(directory.resolve("receipt.bin.partial"))))
            val bytes = description.toByteArray(Charsets.UTF_8)
            save("receipt.json", bytes, bytes.size)
            reason == null
        } catch (_: Exception) {
            fail("dump_failed")
            // Preserve partial evidence; do not publish raw exceptions, retry, or fail the product's OFF.
            false
        }
    }

    @Synchronized
    internal fun failure(): String? = reason

    private fun fail(value: String) { if (reason == null) reason = value }

    private fun validId(value: String): Boolean = runCatching { java.util.UUID.fromString(value).toString() == value }.getOrDefault(false)

    private fun event(size: Int, type: Char, write: DataOutputStream.() -> Unit) {
        if (sealed || reason != null) return
        if (size > metadataLimit - metadata.size()) { fail("metadata_limit"); return }
        output.writeByte(type.code); output.write()
    }

    companion object {
        const val PROPERTY = "replay.diagnostic.logInputRoot"
        private const val MAX_RAW = 8 * 1024 * 1024
        private const val MAX_METADATA = 4 * 1024 * 1024
        private const val MAX_READS = 131072
        private const val MAX_DURATION_NS = 240_000_000_000L

        fun optIn(): LogInputDiagnostic? {
            val value = System.getProperty(PROPERTY) ?: return null
            return try { open(Path.of(value)) } catch (_: Exception) {
                LogInputDiagnostic(null, 0, 0, 0, System::nanoTime, ::writePrivate)
            }
        }

        internal fun open(root: Path, rawLimit: Int = MAX_RAW, metadataLimit: Int = MAX_METADATA,
                          readLimit: Int = MAX_READS, now: () -> Long = System::nanoTime,
                          persist: (Path, ByteArray, Int) -> Unit = ::writePrivate): LogInputDiagnostic {
            require(rawLimit in 1..MAX_RAW && metadataLimit in 1..MAX_METADATA && readLimit in 1..MAX_READS)
            validateRoot(root)
            writePrivate(root.resolve("input.claim"), byteArrayOf(), 0)
            return LogInputDiagnostic(root, rawLimit, metadataLimit, readLimit, now, persist)
        }

        private fun validateRoot(root: Path) {
            require(root.isAbsolute && Files.isDirectory(root, NOFOLLOW_LINKS))
            var parent: Path? = root
            while (parent != null) { require(!Files.isSymbolicLink(parent)); parent = parent.parent }
            val owner = root.fileSystem.userPrincipalLookupService.lookupPrincipalByName(System.getProperty("user.name"))
            require(Files.getOwner(root, NOFOLLOW_LINKS) == owner)
            require(Files.getPosixFilePermissions(root, NOFOLLOW_LINKS) == PosixFilePermissions.fromString("rwx------"))
        }

        private fun writePrivate(path: Path, bytes: ByteArray, count: Int) {
            Files.newByteChannel(path, setOf(CREATE_NEW, WRITE, NOFOLLOW_LINKS),
                PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------"))).use { channel ->
                val data = java.nio.ByteBuffer.wrap(bytes, 0, count)
                while (data.hasRemaining()) channel.write(data)
            }
        }
    }
}

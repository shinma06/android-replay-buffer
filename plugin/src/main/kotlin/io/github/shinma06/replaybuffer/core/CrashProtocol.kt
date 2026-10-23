package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.CharacterCodingException
import java.security.MessageDigest
import java.util.HexFormat

internal data class CrashHeader(
    val process: String?,
    val java: Boolean,
    val pid: Int? = null,
    val first: Int? = null,
    val second: Int? = null,
    val problem: AutoSaveReason? = null,
)

/**
 * Interprets only RuntimeInit's two header lines and am_crash's required typed fields.
 * Recognized malformed candidates retain a problem; parsing alone proves neither identity nor save admission.
 */
internal fun crashHeader(source: DeviceLog): CrashHeader? {
    val raw = source.raw
    if (source.lid == 2) {
        if (raw.size < 4 || ByteBuffer.wrap(raw).order(ByteOrder.LITTLE_ENDIAN).int != 30039) return null
        val buffer = ByteBuffer.wrap(raw).order(ByteOrder.LITTLE_ENDIAN).apply { position(4) }
        var process: String? = null
        var first: Int? = null
        var second: Int? = null
        fun integer(): Int {
            require(buffer.remaining() >= 5 && buffer.get().toInt() == 0)
            return buffer.int
        }
        fun stringBytes(): ByteBuffer {
            require(buffer.remaining() >= 5 && buffer.get().toInt() == 2)
            val length = buffer.int
            require(length >= 0 && length <= buffer.remaining())
            return buffer.slice().apply { limit(length) }.also { buffer.position(buffer.position() + length) }
        }
        return try {
            require(buffer.remaining() >= 2 && buffer.get().toInt() == 3)
            val count = buffer.get().toInt() and 0xff
            // Parse identity even in a legacy eight-field event, but never infer recoverable=0.
            require(count in 3..9)
            first = integer()
            second = integer()
            process = crashProcess(stringBytes())
            require(process != null && count == 9)
            integer()
            val native = stringBytes() == ByteBuffer.wrap("Native crash".toByteArray(Charsets.US_ASCII))
            stringBytes()
            stringBytes()
            integer()
            val recoverable = integer()
            require(!buffer.hasRemaining())
            CrashHeader(process, false, first = first, second = second, problem = when {
                recoverable == 1 -> AutoSaveReason.NOT_A_SUPPORTED_FATAL_EVENT
                recoverable != 0 -> AutoSaveReason.UNKNOWN_FORMAT
                native -> AutoSaveReason.UNSUPPORTED_NATIVE
                !source.lidPresent -> AutoSaveReason.UNKNOWN_FORMAT
                else -> null
            })
        } catch (_: IllegalArgumentException) {
            CrashHeader(process, false, first = first, second = second, problem = AutoSaveReason.UNKNOWN_FORMAT)
        }
    }
    if (source.lid != 4 || raw.size < 3 || raw[0].toInt() != 6) return null
    val tagEnd = raw.indexOf(0, 1)
    if (tagEnd < 0 || !raw.matchesAscii(1, tagEnd, "AndroidRuntime")) return null
    val message = tagEnd + 1
    val prefix = "FATAL EXCEPTION: "
    if (!raw.matchesAscii(message, message + prefix.length, prefix)) return null
    var process: String? = null
    return try {
        require(source.lidPresent && raw.last() == 0.toByte())
        val lineEnd = raw.indexOf(10, message + prefix.length)
        require(lineEnd >= 0 && lineEnd - message - prefix.length in 1..256)
        require((message + prefix.length until lineEnd).none { raw[it] == 0.toByte() || raw[it] == 13.toByte() })
        val secondStart = lineEnd + 1
        require(raw.matchesAscii(secondStart, secondStart + 9, "Process: "))
        val secondEnd = (secondStart until raw.size).firstOrNull { raw[it] == 10.toByte() || raw[it] == 0.toByte() }
            ?: throw IllegalArgumentException()
        require(raw[secondEnd] != 0.toByte() || secondEnd == raw.lastIndex)
        val header = raw.copyOfRange(secondStart + 9, secondEnd)
        val separator = ", PID: ".toByteArray(Charsets.US_ASCII)
        val splits = (0..header.size - separator.size).filter { at ->
            separator.indices.all { header[at + it] == separator[it] }
        }
        require(splits.size == 1)
        val split = splits.single()
        process = crashProcess(ByteBuffer.wrap(header, 0, split))
        require(process != null)
        val digits = header.copyOfRange(split + separator.size, header.size)
        require(digits.isNotEmpty() && digits.size <= 10 && digits.all { it in 48.toByte()..57.toByte() })
        val pid = digits.toString(Charsets.US_ASCII).toIntOrNull()
        require(pid != null && pid > 0)
        CrashHeader(process, true, pid, problem = if (pid != source.pid || source.pid <= 0 ||
            source.uid == null || source.uid !in 0..Int.MAX_VALUE.toLong()) AutoSaveReason.UNKNOWN_IDENTITY else null)
    } catch (_: IllegalArgumentException) {
        CrashHeader(process, true, problem = AutoSaveReason.UNKNOWN_FORMAT)
    }
}

private fun crashProcess(bytes: ByteBuffer): String? = try {
    Charsets.UTF_8.newDecoder().decode(bytes).toString().takeIf { name ->
        name.isNotEmpty() && name.length <= 1024 && name.none { Character.isISOControl(it) || it == '\uFFFD' }
    }
} catch (_: CharacterCodingException) { null }

private fun ByteArray.indexOf(value: Byte, start: Int): Int = (start until size).firstOrNull { this[it] == value } ?: -1

private fun ByteArray.matchesAscii(start: Int, end: Int, value: String): Boolean =
    start >= 0 && end <= size && end - start == value.length && value.indices.all { this[start + it].toInt() == value[it].code }

internal fun selectedProcess(process: String, selected: String): Boolean =
    process == selected || process.startsWith("$selected:") && process.length > selected.length + 1

internal fun crashFingerprint(device: String, boot: String, source: DeviceLog): String {
    val bytes = ByteArrayOutputStream()
    DataOutputStream(bytes).use { out ->
        fun field(value: ByteArray) { out.writeInt(value.size); out.write(value) }
        field(device.toByteArray(Charsets.UTF_8))
        field(boot.toByteArray(Charsets.UTF_8))
        field(ByteBuffer.allocate(32).putInt(source.lid).putLong(source.uid ?: -1).putInt(source.pid)
            .putInt(source.tid).putLong(source.wall).putInt(if (source.lidPresent) 1 else 0).array())
        field(source.raw)
    }
    return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes.toByteArray()))
}

/** Host-monotonic nanosecond interval for the reporter lookup, bound to its capture generation. */
internal data class ReporterPoll(val start: Long, val end: Long, val generation: Long, val pid: Int?) {
    fun fresh(receipt: Long, owner: Long): Boolean = generation == owner && pid != null &&
        start >= 0 && start <= end && end <= receipt && receipt - start <= 2_000_000_000
}

internal fun systemServerPid(ps: String): Int? = ps.lineSequence().drop(1).mapNotNull { line ->
    val fields = line.trim().split(Regex("\\s+"))
    if (fields.size == 3 && fields[1] == "1000" && fields[2] == "system_server")
        fields[0].toIntOrNull()?.takeIf { it > 0 } else null
}.toList().singleOrNull()

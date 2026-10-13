package io.github.shinma06.replaybuffer.core

import java.io.DataInputStream
import java.io.EOFException
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

internal sealed interface VideoPacket {
    data class Session(val width: Int, val height: Int) : VideoPacket
    data class Frame(val pts: Long, val config: Boolean, val key: Boolean, val bytes: ByteArray) : VideoPacket
}

internal fun readVideo(input: DataInputStream, started: () -> Unit = {}): VideoPacket {
    val first = input.readUnsignedByte()
    started()
    val high = (first shl 24) or (input.readUnsignedByte() shl 16) or
        (input.readUnsignedByte() shl 8) or input.readUnsignedByte()
    if (high < 0) {
        require(high == Int.MIN_VALUE || high == Int.MIN_VALUE + 1) { "未知のsession flags" }
        val width = input.readInt()
        val height = input.readInt()
        require(width in 1..8192 && height in 1..8192)
        return VideoPacket.Session(width, height)
    }
    val flags = (high.toLong() shl 32) or (input.readInt().toLong() and 0xffffffffL)
    val length = input.readInt()
    require(length in 1..16 * 1024 * 1024) { "動画packet長が上限外です" }
    val bytes = ByteArray(length)
    input.readFully(bytes)
    return VideoPacket.Frame(flags and ((1L shl 61) - 1), flags and (1L shl 62) != 0L,
        flags and (1L shl 61) != 0L, bytes)
}

internal data class DeviceLog(
    val wall: Long,
    val pid: Int,
    val tid: Int,
    val uid: Long?,
    val lid: Int,
    val priority: Int?,
    val tag: String?,
    val message: String?,
    val raw: ByteArray,
    val decodeStatus: String = "text",
    val lidPresent: Boolean = true,
)

internal fun readLog(input: InputStream): DeviceLog {
    val prefix = input.readNBytes(4)
    if (prefix.size != 4) throw EOFException("logcat headerが途切れました")
    val header = ByteBuffer.wrap(prefix).order(ByteOrder.LITTLE_ENDIAN)
    val length = header.short.toInt() and 0xffff
    val headerSize = header.short.toInt() and 0xffff
    require(headerSize in setOf(20, 24, 28) && length in 1..5120 - headerSize) { "logcat header/長さが未対応です" }
    val rest = input.readNBytes(headerSize - 4 + length)
    if (rest.size != headerSize - 4 + length) throw EOFException("logcat recordが途切れました")
    val b = ByteBuffer.wrap(prefix + rest).order(ByteOrder.LITTLE_ENDIAN)
    b.position(4)
    val pid = b.int
    val tid = b.int
    val sec = b.int.toLong() and 0xffffffffL
    val ns = b.int.toLong() and 0xffffffffL
    require(pid >= 0 && tid >= 0 && ns < 1_000_000_000)
    val lid = if (headerSize >= 24) b.int else 0
    val uid = if (headerSize >= 28) b.int.toLong() and 0xffffffffL else null
    require(lid in 0..7)
    val payload = ByteArray(length)
    b.get(payload)
    val text = lid in setOf(0, 1, 3, 4, 7) && payload.size >= 3
    val end = if (text) payload.indexOf(0, 1) else -1
    val priority = if (text) payload[0].toInt() and 0xff else null
    return DeviceLog(Math.addExact(Math.multiplyExact(sec, 1_000_000_000), ns), pid, tid, uid, lid,
        priority, if (end > 0) payload.copyOfRange(1, end).toString(Charsets.UTF_8) else null,
        if (end > 0) payload.copyOfRange(end + 1, payload.size).toString(Charsets.UTF_8).trimEnd('\u0000') else null,
        payload, if (!text) "binary" else if (end <= 0) "malformed_text" else try {
            Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(payload.copyOfRange(1, payload.size)))
            "text"
        } catch (_: java.nio.charset.CharacterCodingException) { "invalid_utf8" }, headerSize >= 24)
}

private fun ByteArray.indexOf(value: Byte, from: Int): Int {
    for (i in from until size) if (this[i] == value) return i
    return -1
}

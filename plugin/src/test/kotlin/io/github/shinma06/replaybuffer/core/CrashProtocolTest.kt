package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import org.junit.jupiter.api.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class CrashProtocolTest {
    @Test
    fun exactRuntimeHeadersAndTypedEventsPreserveOpaqueModifiedUtf8() {
        val opaque = byteArrayOf(0xc0.toByte(), 0x80.toByte())
        val java = javaCrash(opaque = opaque)
        val platform = platformCrash(opaque = opaque)
        assertEquals("invalid_utf8", java.decodeStatus)
        assertEquals("binary", platform.decodeStatus)
        assertEquals("com.example.target", assertNotNull(crashHeader(java)).process)
        assertNull(crashHeader(java)?.problem)
        assertNull(crashHeader(platform)?.problem)
        assertTrue(java.raw.toList().windowed(2).any { it == opaque.toList() })
        assertNull(crashHeader(javaCrash(process = "com.example.target:worker", thread = "worker"))?.problem)
        assertNull(crashHeader(javaCrash(opaque = "日本語".toByteArray()))?.problem)
        assertEquals(AutoSaveReason.UNSUPPORTED_NATIVE, crashHeader(platformCrash(exception = "Native crash"))?.problem)
        assertEquals(AutoSaveReason.NOT_A_SUPPORTED_FATAL_EVENT, crashHeader(platformCrash(exception = "Native crash", recoverable = 1))?.problem)
        assertEquals(AutoSaveReason.UNKNOWN_FORMAT, crashHeader(platformCrash(recoverable = 2))?.problem)
    }

    @Test
    fun irrelevantErrorsStacksEventsAndWrongTagsDoNotBecomeCandidates() {
        for (message in listOf("ERROR", "handled exception\n at x.y(z:1)", "Process: com.example.target, PID: 4242")) {
            assertNull(crashHeader(javaCrash(message = message)))
        }
        assertNull(crashHeader(javaCrash(tag = "WrongRuntime", opaque = byteArrayOf(0xc0.toByte(), 0x80.toByte()))))
        for (tag in listOf(30040, 30008, 30023, 30039 + 1)) assertNull(crashHeader(platformCrash(eventTag = tag)))
        assertNull(crashHeader(javaCrash(lid = 0)))
    }

    @Test
    fun requiredHeaderIdentityAndFullBinaryFramingAreStrict() {
        val messages = listOf(
            "FATAL EXCEPTION: main",
            "FATAL EXCEPTION: \nProcess: com.example.target, PID: 4242",
            "FATAL EXCEPTION: ${"x".repeat(257)}\nProcess: com.example.target, PID: 4242",
            "FATAL EXCEPTION: ma\rin\nProcess: com.example.target, PID: 4242",
            "FATAL EXCEPTION: main\nProcess: com.example.target, PID: 0",
            "FATAL EXCEPTION: main\nProcess: com.example.target, PID: 2147483648",
            "FATAL EXCEPTION: main\nProcess: com.example.target, PID: 4242 extra",
            "FATAL EXCEPTION: main\nProcess: com.example.target, PID: 1, PID: 4242",
            "FATAL EXCEPTION: main\nProcess: com.example.target, PID: 4242\r",
        )
        messages.forEach { assertEquals(AutoSaveReason.UNKNOWN_FORMAT, crashHeader(javaCrash(message = it))?.problem, it) }
        assertEquals(AutoSaveReason.UNKNOWN_IDENTITY, crashHeader(javaCrash(textPid = 4243))?.problem)
        assertEquals(AutoSaveReason.UNKNOWN_IDENTITY, crashHeader(javaCrash(uid = null))?.problem)
        val badIdentity = javaCrash().raw.let { bytes ->
            val at = bytes.toString(Charsets.UTF_8).indexOf("com.example")
            bytes.copyOfRange(0, at) + byteArrayOf(0xc0.toByte(), 0x80.toByte()) + bytes.copyOfRange(at + 1, bytes.size)
        }
        assertEquals(AutoSaveReason.UNKNOWN_FORMAT, crashHeader(wireLog(badIdentity, 4, 4242, 10123))?.problem)
        val original = platformCrash().raw
        val corruptions = listOf(
            original.copyOf().apply { this[5] = 8 },
            original.copyOf().apply { ByteBuffer.wrap(this).order(ByteOrder.LITTLE_ENDIAN).putInt(17, -1) },
            original.copyOf(original.size - 1),
            original + byteArrayOf(0),
            original.copyOf().apply { this[16] = 3 },
        )
        corruptions.forEach { assertEquals(AutoSaveReason.UNKNOWN_FORMAT, crashHeader(wireLog(it, 2, 500, 1000))?.problem) }
        // Required process framing is separate from attribution: an unreadable process is not a target.
        assertNull(crashHeader(wireLog(corruptions[1], 2, 500, 1000))?.process)
        for (field in listOf(5, 6)) {
            val malformed = eventBytes(opaqueTypeField = field)
            assertEquals(AutoSaveReason.UNKNOWN_FORMAT, crashHeader(wireLog(malformed, 2, 500, 1000))?.problem)
        }
    }

    @Test
    fun reporterRequiresAUniquePositiveSystemUidPidAndConservativePollInterval() {
        assertEquals(500, systemServerPid("PID UID NAME\n500 1000 system_server\n4242 10123 com.example.target"))
        assertNull(systemServerPid("PID UID NAME\n500 1000 system_server\n501 1000 system_server"))
        assertNull(systemServerPid("PID UID NAME\n500 1001 system_server"))
        val poll = ReporterPoll(17_000_000_000, 20_090_000_000, 1, 500)
        assertTrue(!poll.fresh(20_100_000_000, 1))
        assertTrue(!ReporterPoll(20_000_000_000, 20_200_000_000, 1, 500).fresh(20_100_000_000, 1))
        assertTrue(ReporterPoll(18_100_000_000, 20_000_000_000, 1, 500).fresh(20_100_000_000, 1))
        assertTrue(!ReporterPoll(20_000_000_000, 20_050_000_000, 2, 500).fresh(20_100_000_000, 1))
    }
}

internal const val CRASH_WALL = 1_700_000_000_000_000_000L

internal fun wireLog(payload: ByteArray, lid: Int, pid: Int, uid: Long?, wall: Long = CRASH_WALL + 10_000_000_000): DeviceLog {
    val size = if (uid == null) 24 else 28
    val buffer = ByteBuffer.allocate(size + payload.size).order(ByteOrder.LITTLE_ENDIAN)
    buffer.putShort(payload.size.toShort()).putShort(size.toShort()).putInt(pid).putInt(pid)
        .putInt((wall / 1_000_000_000).toInt()).putInt((wall % 1_000_000_000).toInt()).putInt(lid)
    if (uid != null) buffer.putInt(uid.toInt())
    buffer.put(payload)
    return readLog(buffer.array().inputStream())
}

internal fun javaCrash(process: String = "com.example.target", pid: Int = 4242, textPid: Int = pid,
                       uid: Long? = 10123, wall: Long = CRASH_WALL + 10_003_000_000,
                       thread: String = "main", opaque: ByteArray = byteArrayOf(), tag: String = "AndroidRuntime",
                       lid: Int = 4, message: String? = null): DeviceLog {
    val text = message ?: "FATAL EXCEPTION: $thread\nProcess: $process, PID: $textPid\njava.lang.RuntimeException: SYNTHETIC_284"
    return wireLog(byteArrayOf(6) + tag.toByteArray() + byteArrayOf(0) + text.toByteArray() + opaque + byteArrayOf(0), lid, pid, uid, wall)
}

internal fun platformCrash(process: String = "com.example.target", first: Int = 4242, second: Int = 0,
                           pid: Int = 500, uid: Long? = 1000, wall: Long = CRASH_WALL + 10_004_000_000,
                           exception: String = "java.lang.RuntimeException", recoverable: Int = 0,
                           opaque: ByteArray = byteArrayOf(), eventTag: Int = 30039): DeviceLog =
    wireLog(eventBytes(process, first, second, exception, recoverable, opaque, eventTag), 2, pid, uid, wall)

internal fun eventBytes(process: String = "com.example.target", first: Int = 4242, second: Int = 0,
                        exception: String = "java.lang.RuntimeException", recoverable: Int = 0,
                        opaque: ByteArray = byteArrayOf(), eventTag: Int = 30039, opaqueTypeField: Int? = null): ByteArray {
    val out = ByteArrayOutputStream()
    fun int(value: Int) { out.write(ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(value).array()) }
    fun integer(value: Int) { out.write(0); int(value) }
    fun string(value: ByteArray, field: Int) { out.write(if (opaqueTypeField == field) 0 else 2); int(value.size); out.write(value) }
    int(eventTag); out.write(3); out.write(9)
    integer(first); integer(second); string(process.toByteArray(), 2); integer(0)
    string(exception.toByteArray(), 4); string("synthetic".toByteArray() + opaque, 5)
    string("Main".toByteArray() + opaque + ".kt".toByteArray(), 6); integer(42); integer(recoverable)
    return out.toByteArray()
}

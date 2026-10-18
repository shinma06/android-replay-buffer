package io.github.shinma06.replaybuffer.core

import java.io.EOFException
import java.lang.reflect.InvocationTargetException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

class DeviceLogTailTest {
    @Test
    fun liveReaderRetainsBackwardClockRowsAndReconnectExcludesItsOwnOldTail() {
        val root = Files.createTempDirectory("replay-log-tail-")
        val fake = root.resolve("adb")
        Files.writeString(fake, """#!/usr/bin/python3
import pathlib,sys,time
r=pathlib.Path(__file__).parent;a=sys.argv[1:]
if a==['-s','fixture-1','logcat','-b','main','-B','-T','1']:
    sys.stdout.buffer.write((r/'first.bin').read_bytes());sys.stdout.buffer.flush()
    end=time.monotonic()+5
    while not (r/'continue').exists() and time.monotonic()<end: time.sleep(.01)
    if not (r/'continue').exists(): sys.exit(7)
    sys.stdout.buffer.write((r/'after.bin').read_bytes())
elif a==['-s','fixture-1','shell','ps','-A','-o','PID,ARGS']: print('PID ARGS')
elif a[:5]==['-s','fixture-1','shell','rm','-f']: pass
else: sys.exit(9)
""")
        assertTrue(fake.toFile().setExecutable(true))
        val store = CaptureStore(root.resolve("ring"), minFree = 0, captureMode = CaptureMode.LOG_ONLY)
        val resources = CaptureResources(Files.createDirectory(root.resolve("resources")))
        val capture = DeviceCapture(fake, "fixture-1", resources, store, 1, ApplicationTarget())
        val logs = DeviceCapture::class.java.getDeclaredMethod("logs", String::class.java, Boolean::class.javaPrimitiveType)
            .apply { isAccessible = true }
        val stored = CaptureStore::class.java.getDeclaredField("logs").apply { isAccessible = true }
        @Suppress("UNCHECKED_CAST")
        fun rows(): List<LogEntry> = synchronized(store) { (stored.get(store) as Collection<LogEntry>).toList() }
        val wall = 1_700_000_000_000_000_000L
        val host = System.nanoTime() + 60_000_000_000L
        var reader: Thread? = null
        try {
            store.generation(1)
            assertTrue(store.clock.add("fixture-boot", listOf(1_000_000_000L, 1_000_000_000L, wall, 1_000_000_000L), host, host))
            repeat(2) { attempt ->
                val lower = wall - attempt * 10_000_000_000L
                Files.deleteIfExists(root.resolve("continue"))
                Files.write(root.resolve("first.bin"), packet(lower - 1, "old-tail") + packet(lower, "first-live"))
                val back = if (attempt == 0) lower - 10_000_000_000L else lower
                Files.write(root.resolve("after.bin"), packet(back, "same") + packet(wall - 20_000_000_000L, "same"))
                val failure = AtomicReference<Throwable>()
                val currentReader = thread(name = "replay-log-tail-fixture", isDaemon = true) {
                    try { logs.invoke(capture, "main", true) }
                    catch (error: InvocationTargetException) { failure.set(error.cause) }
                    catch (error: Throwable) { failure.set(error) }
                }
                reader = currentReader
                val deadline = System.nanoTime() + 5_000_000_000L
                while (rows().size != attempt * 3 + 1 && currentReader.isAlive && System.nanoTime() < deadline) Thread.sleep(10)
                assertEquals(attempt * 3 + 1, rows().size)
                if (attempt == 0) {
                    // The same reader stays live while the sampled wall clock starts a new epoch.
                    assertTrue(store.clock.add("fixture-boot", listOf(1_001_000_000L, 1_001_000_000L, back, 1_001_000_000L),
                        host + 1_000_000L, host + 1_000_000L))
                    assertEquals(1, store.clock.currentEpoch())
                }
                Files.createFile(root.resolve("continue"))
                currentReader.join(5000)
                assertFalse(currentReader.isAlive)
                assertIs<EOFException>(failure.get())
                val accepted = rows().drop(attempt * 3)
                assertEquals(listOf("first-live", "same", "same"), accepted.map { it.source.message })
                assertEquals(listOf(lower, back, wall - 20_000_000_000L), accepted.map { it.source.wall })
                accepted.forEach { row ->
                    assertEquals(12, row.source.pid)
                    assertEquals(10001L, row.source.uid)
                    assertEquals(0, row.source.lid)
                    assertEquals(1L, row.generation)
                    assertContentEquals(("\u0004Fixture\u0000${row.source.message}\u0000").toByteArray(), row.source.raw)
                }
                assertEquals(listOf(attempt, 1), accepted.take(2).map { it.time.epoch })
                assertTrue(accepted.take(2).all { it.time.elapsed != null })
                assertNull(accepted.last().time.elapsed)
                assertNull(accepted.last().time.sequence)
                assertEquals(Long.MAX_VALUE, accepted.last().time.uncertainty)
            }
            assertEquals(6, rows().map { it.id }.toSet().size)
            assertTrue(rows().zipWithNext().all { (a, b) -> a.host <= b.host })
        } finally {
            capture.close()
            reader?.join(5000)
            assertTrue(reader?.isAlive != true)
            store.close()
            resources.close()
            root.toFile().deleteRecursively()
        }
    }

    private fun packet(wall: Long, message: String): ByteArray {
        val payload = ("\u0004Fixture\u0000$message\u0000").toByteArray()
        return ByteBuffer.allocate(28 + payload.size).order(ByteOrder.LITTLE_ENDIAN)
            .putShort(payload.size.toShort()).putShort(28).putInt(12).putInt(12)
            .putInt((wall / 1_000_000_000).toInt()).putInt((wall % 1_000_000_000).toInt())
            .putInt(0).putInt(10001).put(payload).array()
    }
}

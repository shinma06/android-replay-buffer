package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import java.io.DataOutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.nio.ByteBuffer
import java.nio.file.Files
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/** Synthetic SDK client and wire bytes only. Never invokes installed adb, an IDE or a device. */
class ReplayCoreWireTest {
    @Test
    fun failedRequestRetriesItsOriginalWindowWhileAcquisitionReconfigurationAndReconnectContinue() {
        val root = Files.createTempDirectory("replay-wire-fixture-")
        val devices = root.resolve("devices.txt")
        Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\nfixture-2 device model:Other\n")
        val pids = root.resolve("owned-pids.txt")
        val server = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val clockServer = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val stop = AtomicBoolean()
        val peers = CopyOnWriteArrayList<Thread>()
        val sockets = CopyOnWriteArrayList<java.net.Socket>()
        val videoSockets = CopyOnWriteArrayList<java.net.Socket>()
        val resumeVideo = CountDownLatch(1)
        // Use the same JVM clock as the synthetic video PTS; Python's monotonic origin is platform/version dependent.
        val clockAccept = thread(isDaemon = true, name = "replay-clock-fixture") {
            while (!stop.get()) runCatching {
                val socket = clockServer.accept(); sockets += socket
                peers += thread(isDaemon = true, name = "replay-clock-peer") {
                    runCatching { socket.use {
                        val input = it.getInputStream().bufferedReader()
                        val output = it.getOutputStream().bufferedWriter()
                        while (!stop.get()) {
                            val nonce = input.readLine() ?: break
                            val before = System.nanoTime()
                            val mono = System.nanoTime()
                            val wall = System.currentTimeMillis() * 1_000_000
                            val after = System.nanoTime()
                            output.write("$nonce\t$before\t$mono\t$wall\t$after\n"); output.flush()
                        }
                    } }
                }
            }
        }
        val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }
        val encoded = H264Encoder.createH264Encoder().encodeIDRFrame(picture, ByteBuffer.allocate(65536))
        val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
        val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
            .fold(byteArrayOf()) { acc, nal -> acc + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
        val accept = thread(isDaemon = true, name = "replay-wire-fixture") {
            while (!stop.get()) runCatching {
                val socket = server.accept(); sockets += socket; videoSockets += socket
                val connection = videoSockets.size
                peers += thread(isDaemon = true, name = "replay-wire-peer") {
                    runCatching { socket.use {
                        val out = DataOutputStream(it.getOutputStream())
                        out.writeByte(0); out.writeInt(0x68323634)
                        out.writeInt(Int.MIN_VALUE); out.writeInt(32); out.writeInt(32)
                        out.writeLong(1L shl 62); out.writeInt(config.size); out.write(config)
                        if (connection == 2) assertTrue(resumeVideo.await(15, TimeUnit.SECONDS))
                        Thread.sleep(750) // Exercise a valid clock before the first video frame.
                        while (!stop.get()) {
                            out.writeLong((1L shl 61) or (System.nanoTime() / 1000)); out.writeInt(bytes.size); out.write(bytes); out.flush()
                            Thread.sleep(100)
                        }
                    } }
                }
            }
        }
        val fake = root.resolve("adb")
        Files.writeString(fake, """#!/usr/bin/python3
import sys,time,os,struct,pathlib,socket
root=pathlib.Path(__file__).parent
with (root/'owned-pids.txt').open('a') as f: f.write(str(os.getpid())+'\n')
a=sys.argv[1:]
if a==['devices','-l']:
    print((root/'devices.txt').read_text(),end='')
elif len(a)>2 and a[2]=='push':
    pass
elif len(a)>2 and a[2]=='forward':
    if '--remove' not in a: print(${server.localPort})
elif len(a)>2 and a[2]=='logcat':
    while True:
        epoch=time.time_ns(); payload=b'\x04Fixture\x00hello\x00'
        sys.stdout.buffer.write(struct.pack('<HHiiIIII',len(payload),28,12,12,epoch//1000000000,epoch%1000000000,0,10001)+payload)
        sys.stdout.buffer.flush(); time.sleep(.1)
elif len(a)>3 and 'ClockProbe' in a[3]:
    print('REPLAY_CLOCK_1',flush=True)
    with socket.create_connection(('127.0.0.1',${clockServer.localPort})) as clock:
        reader=clock.makefile('r'); writer=clock.makefile('w')
        for nonce in sys.stdin:
            writer.write(nonce); writer.flush()
            print(reader.readline().strip(),flush=True)
elif len(a)>3 and 'scrcpy.Server' in a[3]:
    time.sleep(30)
elif len(a)>3 and a[3]=='settings':
    print(1)
elif len(a)>3 and a[3]=='cmd':
    print('package:com.fixture.app uid:10001')
elif len(a)>3 and a[3]=='ps':
    print('PID UID NAME\n12 10001 com.fixture.app' if a[-1]=='PID,UID,NAME' else 'PID ARGS')
""")
        assertTrue(fake.toFile().setExecutable(true))
        val core = ReplayCore(ReplaySettings(fake, root.resolve("missing"), 1,
            ApplicationTarget("com.fixture.app", ApplicationMode.MANUAL)), root.resolve("workspace"))
        fun awaitState(phase: String, predicate: (ReplaySnapshot) -> Boolean): ReplaySnapshot {
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(15)
            while (System.nanoTime() < deadline) {
                val state = core.snapshot()
                if (predicate(state)) return state
                Thread.sleep(25)
            }
            val state = core.snapshot()
            error("fixture deadline ($phase): ${state.captureState}/${state.save.phase}/frozen=${state.frozen}/video=${state.video}/deviceLog=${state.deviceLog}/appLog=${state.appLog}/error=${state.error}")
        }
        try {
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            awaitState("unselected-multiple") { it.captureState == CaptureState.MULTIPLE_DEVICES && it.device == null }
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\n")
            val capturing = awaitState("initial-capture") { it.canSave && it.video.state == StreamState.CAPTURING && it.appLog.state == StreamState.CAPTURING }
            val sequence = capturing.sequenceId
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\nfixture-2 device model:Other\n")
            val multiple = awaitState("selected-multiple") { it.captureState == CaptureState.MULTIPLE_DEVICES && it.device != null }
            assertFalse(multiple.frozen)
            assertEquals(capturing.generation, multiple.generation)
            assertEquals(StreamState.CAPTURING, multiple.video.state)
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\n")
            sockets.first { it.localPort == server.localPort }.close()
            awaitState("video-interrupted") { it.video.state != StreamState.CAPTURING }
            resumeVideo.countDown()
            awaitState("video-recovered") { videoSockets.size >= 2 && it.video.state == StreamState.CAPTURING }
            val operation = core.save().get(10, TimeUnit.SECONDS)
            assertTrue(operation.accepted)
            val failed = awaitState("save-failed") { it.save.phase == SavePhase.FAILED }
            val fixed = failed.save
            assertTrue("video" in fixed.missingKinds)
            assertEquals(StreamState.CAPTURING, failed.video.state)
            assertFalse(core.save().get().accepted)
            assertFalse(core.retry("stale-id").get().accepted)
            assertTrue(core.applySettings(core.snapshot().settings.copy(replaySeconds = 2)).get().accepted)
            assertEquals(sequence, core.snapshot().sequenceId)
            Files.writeString(devices, "List of devices attached\n")
            val disconnected = awaitState("disconnected") { it.frozen }
            Thread.sleep(1100)
            assertEquals(disconnected.windowEndNs, core.snapshot().windowEndNs)
            assertEquals(fixed.windowEndNs, core.snapshot().save.windowEndNs)
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\n")
            val resumed = awaitState("reconnected") { !it.frozen && it.generation > capturing.generation && it.video.state == StreamState.CAPTURING }
            assertEquals(sequence, resumed.sequenceId)
            assertTrue(core.retryAtDirectory(operation.requestId!!, root).get().accepted)
            val done = awaitState("save-completed") { it.save.phase == SavePhase.COMPLETED }
            assertEquals(fixed.windowEndNs, done.save.windowEndNs)
            val folder = done.save.directory!!
            val json = JsonParser.parseString(Files.readString(folder.resolve("session.json"))).asJsonObject
            assertEquals(operation.requestId, json["save_id"].asString)
            assertEquals(1, json["replay_seconds"].asInt)
            assertTrue(json["clock_samples"].asJsonArray.any { it.asJsonObject["valid"].asBoolean })
            val frames = Files.readAllLines(folder.resolve("frames.jsonl")).map { JsonParser.parseString(it).asJsonObject }
            assertTrue(frames.isNotEmpty())
            assertTrue(frames.all { !it["elapsed_ns"].isJsonNull && !it["window_ns"].isJsonNull })
            val parts = json["parts"].asJsonArray
            assertTrue(parts.size() > 0)
            parts.forEach { part ->
                val item = part.asJsonObject
                assertTrue(item["edit_start_us"].asString.toLong() >= 0)
                assertTrue(!item["window_start_ns"].isJsonNull)
                assertTrue(Files.size(folder.resolve(item["file"].asString)) > 0)
            }
            val hash = sha256(folder.resolve("session.json"))
            assertTrue(core.setEnabled(false).get(15, TimeUnit.SECONDS).accepted)
            core.closeAsync().get(15, TimeUnit.SECONDS)
            assertEquals(hash, sha256(folder.resolve("session.json")))
            assertEquals(0, Files.list(root.resolve("workspace")).use { it.count() })
            Files.readAllLines(pids).forEach { pid -> assertFalse(ProcessHandle.of(pid.toLong()).map { it.isAlive }.orElse(false)) }
        } finally {
            core.closeAsync().get(15, TimeUnit.SECONDS)
            stop.set(true); resumeVideo.countDown(); server.close(); clockServer.close(); sockets.forEach { runCatching { it.close() } }
            accept.join(2000); clockAccept.join(2000); peers.forEach { it.join(2000) }
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }
}

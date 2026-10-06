package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.condition.EnabledOnOs
import org.junit.jupiter.api.condition.OS
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
        runWireFixture(false)
    }

    @Test
    fun anInitialDiscardedPFrameStillTimesOutAndTheRecoveredIdrSurvivesNormalIdle() {
        runWireFixture(true)
    }

    @Test
    @EnabledOnOs(OS.MAC) // Existing SaveWriter fixture uses macOS AVFoundation; not IDE/device acceptance.
    fun partitionedLogsPreserveIdsAndPartialFailureAndLegacyUnknownHeadersKeepAll() {
        for (mode in listOf("partition", "legacy", "empty", "timeout", "cancel")) runWireFixture(false, mode)
    }

    private fun runWireFixture(discardInitialPFrame: Boolean, logMode: String? = null) {
        val root = Files.createTempDirectory("replay-wire-fixture-")
        Files.writeString(root.resolve("probe-mode.txt"), logMode ?: "partition")
        Files.writeString(root.resolve("log-events.txt"), "")
        val devices = root.resolve("devices.txt")
        Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\nfixture-2 device model:Other\n")
        val pids = root.resolve("owned-pids.txt")
        val server = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val clockServer = ServerSocket(0, 8, InetAddress.getLoopbackAddress())
        val stop = AtomicBoolean()
        val pauseVideo = AtomicBoolean()
        val resetConfig = AtomicBoolean()
        val repeatRejectedPackets = AtomicBoolean()
        val resetConfigSent = CountDownLatch(1)
        val peers = CopyOnWriteArrayList<Thread>()
        val sockets = CopyOnWriteArrayList<java.net.Socket>()
        val videoSockets = CopyOnWriteArrayList<java.net.Socket>()
        val resumeVideo = CountDownLatch(1)
        val discardedFrameSent = CountDownLatch(1)
        val readyConnections = java.util.concurrent.atomic.AtomicInteger()
        var bindReadyAt = 0L
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
        val encoder = H264Encoder.createH264Encoder()
        val encoded = encoder.encodeFrame(picture, ByteBuffer.allocate(65536)).data
        val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
        picture.fill(25)
        val encodedP = encoder.encodeFrame(picture, ByteBuffer.allocate(65536)).data
        val pBytes = ByteArray(encodedP.remaining()).also { encodedP.get(it) }
        assertTrue(H264Utils.splitFrame(ByteBuffer.wrap(pBytes)).any { it.get(0).toInt() and 31 == 1 })
        val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
            .fold(byteArrayOf()) { acc, nal -> acc + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
        val changedConfig = config + byteArrayOf(0) // Valid Annex B trailing padding changes the store config identity.
        val accept = thread(isDaemon = true, name = "replay-wire-fixture") {
            while (!stop.get()) runCatching {
                val socket = server.accept(); sockets += socket; videoSockets += socket
                if (bindReadyAt == 0L) bindReadyAt = System.nanoTime() + 500_000_000
                if (System.nanoTime() < bindReadyAt) { socket.close(); continue }
                val connection = readyConnections.incrementAndGet()
                peers += thread(isDaemon = true, name = "replay-wire-peer") {
                    runCatching { socket.use {
                        val out = DataOutputStream(it.getOutputStream())
                        out.writeByte(0); out.writeInt(0x68323634)
                        out.writeInt(Int.MIN_VALUE); out.writeInt(32); out.writeInt(32)
                        out.writeLong(1L shl 62); out.writeInt(config.size); out.write(config)
                        if (discardInitialPFrame && connection == 1) {
                            out.writeLong(System.nanoTime() / 1000); out.writeInt(pBytes.size); out.write(pBytes); out.flush()
                            discardedFrameSent.countDown()
                            assertEquals(-1, it.getInputStream().read()) // Only the client watchdog ends this healthy idle.
                            return@use
                        }
                        if (connection == 2) assertTrue(resumeVideo.await(15, TimeUnit.SECONDS))
                        if (discardInitialPFrame && connection >= 3) { repeatRejectedPackets.set(false); pauseVideo.set(false) }
                        Thread.sleep(750) // Exercise a valid clock before the first video frame.
                        while (!stop.get()) {
                            if (resetConfig.compareAndSet(true, false)) {
                                out.writeLong(1L shl 62); out.writeInt(changedConfig.size); out.write(changedConfig); out.flush()
                                repeatRejectedPackets.set(true); resetConfigSent.countDown()
                            }
                            if (repeatRejectedPackets.get()) {
                                out.writeLong(1L shl 62); out.writeInt(changedConfig.size); out.write(changedConfig)
                                out.writeLong(System.nanoTime() / 1000); out.writeInt(pBytes.size); out.write(pBytes); out.flush()
                                Thread.sleep(200); continue
                            }
                            if (pauseVideo.get()) { Thread.sleep(25); continue }
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
if a==['forward','--list']:
    pass
elif a==['devices','-l']:
    print((root/'devices.txt').read_text(),end='')
elif len(a)>2 and a[2]=='push':
    pass
elif len(a)>2 and a[2]=='forward':
    if '--remove' not in a: print(${server.localPort})
elif len(a)>2 and a[2]=='logcat':
    mode=(root/'probe-mode.txt').read_text()
    buffer=a[a.index('-b')+1]
    def emit(lid,message,epoch=None,pid=12,uid=10001,header=28,tag='Fixture'):
        epoch=time.time_ns() if epoch is None else epoch
        payload=(b'\x06'+message.encode()) if lid==2 else b'\x04'+tag.encode()+b'\x00'+message.encode()+b'\x00'
        raw=struct.pack('<HHiiII',len(payload),header,pid,pid,epoch//1000000000,epoch%1000000000)
        if header>=24: raw+=struct.pack('<I',lid)
        if header>=28: raw+=struct.pack('<I',uid)
        raw+=payload
        for i in range(0,len(raw),3):
            sys.stdout.buffer.write(raw[i:i+3]); sys.stdout.buffer.flush()
    if '-d' in a:
        (root/'probe-started').touch()
        if mode=='empty': sys.exit(0)
        if mode in ('timeout','cancel'): time.sleep(90)
        emit(0,'probe',header=20 if mode=='legacy' else 28)
        sys.exit(0)
    with (root/'log-launches.txt').open('a') as f: f.write(buffer+'\n')
    seen=len((root/'log-events.txt').read_text().splitlines())
    while True:
        if (root/('fail-'+buffer)).exists(): sys.exit(7)
        if buffer=='all' and (root/'wrong-header-all').exists():
            emit(0,'wrong header',header=20); time.sleep(.1); continue
        if buffer=='all' and (root/'quiet-all').exists(): time.sleep(.1); continue
        emit(0,'hello',header=20 if mode=='legacy' else 28)
        lines=(root/'log-events.txt').read_text().splitlines()
        for line in lines[seen:]:
            wall,label=line.split(' ',1); wall=int(wall)
            emit(0,label,wall,header=20 if mode=='legacy' else 28,tag='Partition')
            if buffer=='all' and mode!='legacy':
                emit(1,label,wall,tag='Partition')
                emit(2,label,wall,pid=99,uid=10002)
                emit(3,label,wall,pid=99,header=24,tag='Partition')
            (root/(buffer+'-'+label+'.sent')).touch()
        seen=len(lines)
        time.sleep(.1)
elif len(a)>3 and 'ClockProbe' in a[3]:
    print('REPLAY_CLOCK_1',flush=True)
    with socket.create_connection(('127.0.0.1',${clockServer.localPort})) as clock:
        reader=clock.makefile('r'); writer=clock.makefile('w')
        for nonce in sys.stdin:
            writer.write(nonce); writer.flush()
            print(reader.readline().strip(),flush=True)
elif len(a)>3 and 'scrcpy.Server' in a[3]:
    with (root/'video-launches.txt').open('a') as f: f.write('start\n')
    time.sleep(90)
elif len(a)>3 and a[3]=='settings':
    print(1)
elif len(a)>3 and a[3]=='cmd':
    print('package:com.fixture.app uid:10001')
elif len(a)>3 and a[3]=='ps':
    print('PID UID NAME\n12 10001 com.fixture.app' if a[-1]=='PID,UID,NAME' else 'PID ARGS')
""")
        assertTrue(fake.toFile().setExecutable(true))
        val core = ReplayCore(ReplaySettings(fake, root.resolve("missing"), 1,
            ApplicationTarget()), root.resolve("workspace"))
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
            val enabled = core.setEnabled(true).get(10, TimeUnit.SECONDS)
            assertTrue(enabled.accepted, enabled.reason)
            awaitState("unselected-multiple") { it.captureState == CaptureState.MULTIPLE_DEVICES && it.device == null }
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\n")
            if (logMode == "cancel") {
                val deadline = System.nanoTime() + 3_000_000_000
                while (!Files.exists(root.resolve("probe-started")) && System.nanoTime() < deadline) Thread.sleep(10)
                assertTrue(Files.exists(root.resolve("probe-started")))
                val started = System.nanoTime()
                assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
                assertTrue(System.nanoTime() - started < 5_500_000_000)
                core.closeAsync().get(10, TimeUnit.SECONDS)
                assertFalse(Files.exists(root.resolve("log-launches.txt")))
                Files.readAllLines(pids).forEach { pid -> assertFalse(ProcessHandle.of(pid.toLong()).map { it.isAlive }.orElse(false)) }
                return
            }
            if (discardInitialPFrame) {
                assertTrue(discardedFrameSent.await(5, TimeUnit.SECONDS))
                awaitState("discarded-initial-P-frame") { it.video.state == StreamState.RECOVERING && it.video.gaps.any { gap ->
                    gap.reason == "IDR待ちでframeを保持できません"
                } }
                resumeVideo.countDown()
            }
            awaitState("unresolved-app") { it.video.state == StreamState.CAPTURING && it.appLog.state == StreamState.UNAVAILABLE }
            assertTrue(core.updateApplication(ApplicationTarget("com.fixture.app")).get().accepted)
            val capturing = awaitState("initial-capture") { it.canSave && it.video.state == StreamState.CAPTURING && it.appLog.state == StreamState.CAPTURING }
            if (logMode != null) {
                assertTrue(core.applySettings(core.snapshot().settings.copy(replaySeconds = 30, saveDirectory = root)).get().accepted)
                fun sent(buffer: String, label: String) {
                    val deadline = System.nanoTime() + 3_000_000_000
                    while (!Files.exists(root.resolve("$buffer-$label.sent")) && System.nanoTime() < deadline) Thread.sleep(10)
                    assertTrue(Files.exists(root.resolve("$buffer-$label.sent")))
                }
                fun emit(label: String, buffer: String = if (logMode == "partition") "main" else "all") {
                    Files.writeString(root.resolve("log-events.txt"), "${System.currentTimeMillis() * 1_000_000} $label\n",
                        java.nio.file.StandardOpenOption.APPEND)
                    sent(buffer, label)
                }
                emit("first")
                sent("all", "first")
                if (logMode == "partition") {
                    val quietGaps = core.snapshot().deviceLog.gaps
                    Files.createFile(root.resolve("quiet-all"))
                    Thread.sleep(1200)
                    assertEquals(StreamState.CAPTURING, core.snapshot().deviceLog.state)
                    assertEquals(quietGaps, core.snapshot().deviceLog.gaps)
                    Files.delete(root.resolve("quiet-all"))
                    Files.createFile(root.resolve("fail-all"))
                    awaitState("all-only-failure") { it.deviceLog.state == StreamState.RECOVERING }
                    emit("partial")
                    val partial = core.snapshot()
                    assertEquals(StreamState.RECOVERING, partial.deviceLog.state)
                    assertTrue(partial.deviceLog.gaps.any { it.toNs == null })
                    Files.delete(root.resolve("fail-all"))
                    awaitState("all-recovered") { it.deviceLog.state == StreamState.CAPTURING }
                    Files.createFile(root.resolve("wrong-header-all"))
                    awaitState("all-header-contradiction") { it.deviceLog.state == StreamState.RECOVERING }
                    emit("header-partial")
                    assertEquals(StreamState.RECOVERING, core.snapshot().deviceLog.state)
                    Files.delete(root.resolve("wrong-header-all"))
                    awaitState("all-header-recovered") { it.deviceLog.state == StreamState.CAPTURING }
                    assertEquals(1, Files.readAllLines(root.resolve("log-launches.txt")).count { it == "main" })
                    Files.createFile(root.resolve("fail-main"))
                    awaitState("main-only-failure") { it.deviceLog.state == StreamState.RECOVERING }
                    emit("main-partial", "all")
                    assertEquals(StreamState.RECOVERING, core.snapshot().deviceLog.state)
                    Files.delete(root.resolve("fail-main"))
                    awaitState("main-recovered") { it.deviceLog.state == StreamState.CAPTURING }
                    emit("recovered")
                    sent("all", "recovered")
                }
                // A subsequent supported clock sample maps all returned fixture records.
                Thread.sleep(1200)
                assertTrue(core.save().get().accepted)
                val saved = awaitState("partition-save") { it.save.phase == SavePhase.COMPLETED }
                val deviceRows = Files.readAllLines(saved.save.directory!!.resolve("logcat-device.jsonl"))
                    .map { JsonParser.parseString(it).asJsonObject }
                val rows = deviceRows.filter { !it["tag"].isJsonNull && it["tag"].asString == "Partition" }
                val appRows = Files.readAllLines(saved.save.directory.resolve("logcat-app.jsonl"))
                    .map { JsonParser.parseString(it).asJsonObject }
                    .filter { !it["tag"].isJsonNull && it["tag"].asString == "Partition" }
                val labels = if (logMode == "partition") listOf("first", "partial", "header-partial", "main-partial", "recovered") else listOf("first")
                labels.forEach { label ->
                    val actual = rows.filter { it["message"].asString == label }
                    val lids = when {
                        logMode == "legacy" || label in listOf("partial", "header-partial") -> listOf(0)
                        label == "main-partial" -> listOf(1, 3)
                        else -> listOf(0, 1, 3)
                    }
                    assertEquals(lids, actual.map { it["lid"].asInt }.sorted())
                    assertEquals(actual.size, actual.map { it["record_id"].asString }.toSet().size)
                    actual.forEach { row ->
                        val id = row["record_id"].asString
                        if (row["lid"].asInt == 3) {
                            assertTrue(row["app_membership"].isJsonNull)
                            assertTrue(appRows.none { it["record_id"].asString == id })
                        } else assertEquals(1, appRows.count { it["record_id"].asString == id })
                    }
                }
                val binary = deviceRows.filter { it["lid"].asInt == 2 }
                assertEquals(if (logMode == "legacy") 0 else if (logMode == "partition") 3 else 1, binary.size)
                assertTrue(binary.all { it["decode_status"].asString == "binary" && !it["app_membership"].asBoolean })
                val launches = Files.readAllLines(root.resolve("log-launches.txt"))
                if (logMode == "partition") assertTrue(launches.count { it == "main" } >= 2)
                else assertEquals(0, launches.count { it == "main" })
                assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
                core.closeAsync().get(10, TimeUnit.SECONDS)
                Files.readAllLines(pids).forEach { pid -> assertFalse(ProcessHandle.of(pid.toLong()).map { it.isAlive }.orElse(false)) }
                return
            }
            if (discardInitialPFrame) {
                assertEquals(2, readyConnections.get())
                pauseVideo.set(true)
                Thread.sleep(11_000)
                assertEquals(2, readyConnections.get())
                assertEquals(StreamState.CAPTURING, core.snapshot().video.state)
                resetConfig.set(true)
                assertTrue(resetConfigSent.await(5, TimeUnit.SECONDS))
                awaitState("changed-config-awaiting-IDR") { it.video.state == StreamState.RECOVERING }
                awaitState("changed-config-recovered") { readyConnections.get() == 3 && it.video.state == StreamState.CAPTURING }
                assertTrue(core.setEnabled(false).get(15, TimeUnit.SECONDS).accepted)
                core.closeAsync().get(15, TimeUnit.SECONDS)
                Files.readAllLines(pids).forEach { pid -> assertFalse(ProcessHandle.of(pid.toLong()).map { it.isAlive }.orElse(false)) }
                return
            }
            if (System.getProperty("os.name") == "Mac OS X") {
                Thread.sleep(1300) // Remove the initial unresolved-app interval from the one-second window.
                pauseVideo.set(true)
                Thread.sleep(11_000)
                assertEquals(1, readyConnections.get())
                assertEquals(StreamState.CAPTURING, core.snapshot().video.state)
                assertTrue(core.snapshot().video.gaps.isEmpty())
                assertTrue(core.applySettings(core.snapshot().settings.copy(saveDirectory = root)).get().accepted)
                repeat(2) {
                    assertTrue(core.save().get().accepted)
                    val normal = awaitState("normal-save") { it.save.phase == SavePhase.COMPLETED }
                    assertTrue(normal.save.missingKinds.isEmpty())
                    assertTrue(normal.canSave)
                    assertEquals(StreamState.CAPTURING, normal.video.state)
                    val tail = normal.save.videoTail!!
                    assertTrue(tail.displayHeld)
                    assertTrue(tail.toNs!! > tail.fromNs!!)
                    val manifest = JsonParser.parseString(Files.readString(normal.save.directory!!.resolve("session.json"))).asJsonObject
                    assertTrue(manifest["complete"].asBoolean)
                    assertFalse(manifest["video_tail"].asJsonObject["new_frame_confirmed"].asBoolean)
                    pauseVideo.set(false)
                    awaitState("static-resumed") { it.video.availableSeconds > 0 && it.video.gaps.isEmpty() }
                }
                assertTrue(core.applySettings(core.snapshot().settings.copy(saveDirectory = root.resolve("missing"))).get().accepted)
            }
            assertEquals(1, Files.readAllLines(root.resolve("video-launches.txt")).size)
            assertTrue(videoSockets.size > 1) // Early forwarded EOFs did not restart the owned server.
            val sequence = capturing.sequenceId
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\nfixture-2 device model:Other\n")
            val multiple = awaitState("selected-multiple") { it.captureState == CaptureState.MULTIPLE_DEVICES && it.device != null }
            assertFalse(multiple.frozen)
            assertEquals(capturing.generation, multiple.generation)
            assertEquals(StreamState.CAPTURING, multiple.video.state)
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\n")
            videoSockets.last().close()
            awaitState("video-interrupted") { it.video.state != StreamState.CAPTURING }
            resumeVideo.countDown()
            awaitState("video-recovered") { readyConnections.get() >= 2 && it.video.state == StreamState.CAPTURING }
            val operation = core.save().get(10, TimeUnit.SECONDS)
            assertTrue(operation.accepted)
            val failed = awaitState("save-failed") { it.save.phase == SavePhase.FAILED }
            val fixed = failed.save
            assertEquals("fixture-1", fixed.device?.serial)
            assertEquals("com.fixture.app", fixed.application?.packageName)
            assertTrue(fixed.applicationHistory.any { it.packageName == "com.fixture.app" && it.resolved })
            assertTrue(failed.video.gaps.isNotEmpty())
            assertTrue("video" in fixed.missingKinds)
            assertEquals(StreamState.CAPTURING, failed.video.state)
            assertFalse(core.save().get().accepted)
            assertFalse(core.retry("stale-id").get().accepted)
            assertTrue(core.applySettings(core.snapshot().settings.copy(replaySeconds = 2)).get().accepted)
            assertEquals(sequence, core.snapshot().sequenceId)
            assertTrue(core.updateApplication(ApplicationTarget("com.other.app")).get().accepted)
            assertEquals(fixed.application, core.snapshot().save.application)
            assertEquals(fixed.applicationHistory, core.snapshot().save.applicationHistory)
            assertEquals(fixed.videoTail, core.snapshot().save.videoTail)
            Files.writeString(devices, "List of devices attached\n")
            val disconnected = awaitState("disconnected") { it.frozen }
            Thread.sleep(1100)
            assertEquals(disconnected.windowEndNs, core.snapshot().windowEndNs)
            assertEquals(fixed.windowEndNs, core.snapshot().save.windowEndNs)
            Files.writeString(devices, "List of devices attached\nfixture-1 device model:Fixture\n")
            val resumed = awaitState("reconnected") { !it.frozen && it.generation > capturing.generation && it.video.state == StreamState.CAPTURING }
            assertEquals(sequence, resumed.sequenceId)
            assertTrue(core.retryAtDirectory(operation.requestId!!, root).get().accepted)
            var completed: java.nio.file.Path? = null
            var savedHash: String? = null
            if (System.getProperty("os.name") == "Mac OS X") {
                val done = awaitState("save-completed") { it.save.phase == SavePhase.COMPLETED }
                assertEquals(fixed.windowEndNs, done.save.windowEndNs)
                assertEquals(fixed.device, done.save.device)
                assertEquals(fixed.application, done.save.application)
                assertEquals(fixed.applicationHistory, done.save.applicationHistory)
                assertEquals(fixed.videoTail, done.save.videoTail)
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
                assertEquals(setOf("video-001.mp4"), parts.map { it.asJsonObject["file"].asString }.toSet())
                Files.list(folder).use { paths -> assertEquals(1L, paths.filter { it.fileName.toString().endsWith(".mp4") }.count()) }
                parts.forEach { part ->
                    val item = part.asJsonObject
                    assertTrue(item["edit_start_us"].asString.toLong() >= 0)
                    // Individual frame mappings survive a source run spanning clock epochs.
                    if (item["clock_alignment_known"].asBoolean) assertFalse(item["window_start_ns"].isJsonNull)
                    else assertTrue(item["window_start_ns"].isJsonNull && item["window_end_ns"].isJsonNull)
                    assertTrue(Files.size(folder.resolve(item["file"].asString)) > 0)
                }
                completed = folder
                savedHash = sha256(folder.resolve("session.json"))
            } else {
                val unsupported = awaitState("unsupported-native-save") { it.save.phase == SavePhase.FAILED }
                assertEquals(fixed.applicationHistory, unsupported.save.applicationHistory)
                assertTrue(core.discard(operation.requestId).get().accepted)
            }
            assertTrue(core.setEnabled(false).get(15, TimeUnit.SECONDS).accepted)
            core.closeAsync().get(15, TimeUnit.SECONDS)
            completed?.let { assertEquals(savedHash, sha256(it.resolve("session.json"))) }
            assertEquals(0, Files.list(root.resolve("workspace")).use { it.count() })
            Files.readAllLines(pids).forEach { pid -> assertFalse(ProcessHandle.of(pid.toLong()).map { it.isAlive }.orElse(false)) }
        } finally {
            core.closeAsync().get(15, TimeUnit.SECONDS)
            stop.set(true); pauseVideo.set(false); resumeVideo.countDown(); server.close(); clockServer.close(); sockets.forEach { runCatching { it.close() } }
            accept.join(2000); clockAccept.join(2000); peers.forEach { it.join(2000) }
            Files.walk(root).use { paths -> paths.sorted(java.util.Comparator.reverseOrder()).forEach { Files.deleteIfExists(it) } }
        }
    }
}

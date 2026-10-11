package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.channels.FileLock
import java.nio.channels.FileChannel
import java.nio.file.attribute.PosixFilePermissions
import java.time.Instant
import java.util.UUID
import java.util.concurrent.TimeUnit

/** A bounded JVM fixture exits without close; no IDE, adb, device, or user process is touched. */
object LocalCaptureOwnerFixture {
    @JvmStatic
    fun main(args: Array<String>) {
        val ownership = LocalCaptureOwnership.create(Path.of(args[0]))
        CaptureResources(ownership.root, ownership::created)
        val store = CaptureStore(ownership.root.resolve("ring"), minFree = 0, created = ownership::created)
        store.generation(1)
        store.session(VideoPacket.Session(32, 32), 1)
        store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
        check(store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1))
        if (args[1] == "unlocked") ownership.close()
        println(ownership.root.fileName)
        System.out.flush()
        if (args[1] != "stopped") System.`in`.read()
    }
}

class LocalCaptureOwnershipTest {
    @TempDir lateinit var directory: Path

    private data class Fixture(val process: Process, val root: Path, val descriptor: Path) : AutoCloseable {
        override fun close() {
            if (process.isAlive) {
                process.outputStream.write(10)
                process.outputStream.flush()
                assertTrue(process.waitFor(10, TimeUnit.SECONDS))
            }
        }
    }

    private fun fixture(namespace: Path, mode: String = "stopped"): Fixture {
        fun location(resource: java.net.URL, suffix: String): String =
            if (resource.protocol == "jar") Path.of(java.net.URI.create(resource.toString().removePrefix("jar:").substringBefore("!/"))).toString()
            else Path.of(resource.toURI()).toString().removeSuffix(suffix)
        val locations = listOf(LocalCaptureOwnerFixture::class.java, LocalCaptureOwnership::class.java,
            kotlin.Unit::class.java, com.google.gson.Gson::class.java).map {
            val suffix = "/${it.name.replace('.', '/')}.class"
            location(it.getResource(suffix)!!, suffix)
        } + location(LocalCaptureOwnership::class.java.getResource("/replay/deps/identity.properties")!!, "/replay/deps/identity.properties")
        val process = ProcessBuilder(Path.of(System.getProperty("java.home"), "bin", "java").toString(),
            "-cp", locations.distinct().joinToString(java.io.File.pathSeparator),
            LocalCaptureOwnerFixture::class.java.name, namespace.toString(), mode).redirectError(ProcessBuilder.Redirect.INHERIT).start()
        val line = java.util.concurrent.CompletableFuture.supplyAsync { process.inputStream.bufferedReader().readLine() }
        try {
            val root = namespace.resolve(line.get(10, TimeUnit.SECONDS) ?: error("fixture failed"))
            val fixture = Fixture(process, root, namespace.resolve("local-capture-${root.fileName.toString().removePrefix("replay-")}.json"))
            if (mode == "stopped") {
                assertTrue(process.waitFor(10, TimeUnit.SECONDS))
                assertEquals(0, process.exitValue())
                assertTrue(ProcessHandle.of(process.pid()).isEmpty)
            }
            return fixture
        } catch (failure: Throwable) {
            process.outputStream.close()
            process.waitFor(10, TimeUnit.SECONDS)
            throw failure
        }
    }

    private fun packet(root: Path) = Files.list(root.resolve("ring")).use { it.findFirst().orElseThrow() }
    private fun namespace(name: String) = Files.createDirectory(directory.resolve(name),
        PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))

    @Test
    fun stoppedOwnerIsReclaimedAndOtherProjectAndCompletedOutputRemain() {
        val a = namespace("a")
        val b = namespace("b")
        val completed = Files.writeString(a.resolve("completed-output.txt"), "saved output")
        fixture(a).use { old -> fixture(b).use { other ->
            val core = ReplayCore(ReplaySettings(), a)
            try { assertTrue(core.applySettings(ReplaySettings()).get(10, TimeUnit.SECONDS).accepted) }
            finally { core.closeAsync().get(10, TimeUnit.SECONDS) }
            assertFalse(Files.exists(old.root))
            assertFalse(Files.exists(old.descriptor))
            assertEquals("saved output", Files.readString(completed))
            assertTrue(Files.exists(other.root))
            assertTrue(Files.exists(other.descriptor))
        } }
    }

    @Test
    fun liveLockAndLiveProcessWithReleasedLockAreBothPreserved() {
        for (mode in listOf("locked", "unlocked")) {
            val namespace = namespace(mode)
            fixture(namespace, mode).use { live ->
                LocalCaptureOwnership.recover(namespace)
                assertTrue(live.process.isAlive)
                assertTrue(Files.exists(packet(live.root)))
                assertTrue(Files.exists(live.descriptor))
            }
            LocalCaptureOwnership.recover(namespace)
            assertEquals(0, Files.list(namespace).use { it.count() })
        }
    }

    @Test
    fun absentMalformedAndAmbiguousOwnerIdentityArePreserved() {
        for (kind in listOf("absent", "truncated", "duplicate", "pid", "started", "namespace", "entries")) {
            val namespace = namespace(kind)
            fixture(namespace).use { old ->
                val before = Files.readAllBytes(packet(old.root))
                if (kind == "absent") Files.delete(old.descriptor)
                else if (kind == "truncated") Files.writeString(old.descriptor, "{broken")
                else if (kind == "duplicate") Files.writeString(old.descriptor, Files.readString(old.descriptor).replaceFirst("{", "{\"schema\":1,"))
                else {
                    val json = JsonParser.parseString(Files.readString(old.descriptor)).asJsonObject
                    when (kind) {
                        "pid", "started" -> json.remove(kind)
                        "namespace" -> json.addProperty(kind, "different project")
                        else -> json.getAsJsonObject("entries").remove(".")
                    }
                    Files.writeString(old.descriptor, json.toString())
                }
                LocalCaptureOwnership.recover(namespace)
                assertArrayEquals(before, Files.readAllBytes(packet(old.root)))
                if (kind != "absent") assertTrue(Files.exists(old.descriptor))
            }
        }
    }

    @Test
    fun unknownEntryAndReplacedPacketArePreservedBeforeAnyDeletion() {
        for (kind in listOf("foreign", "changed")) {
            val namespace = namespace(kind)
            fixture(namespace).use { old ->
                val file = packet(old.root)
                val marker = if (kind == "foreign") Files.writeString(old.root.resolve("session.json"), "complete saved output")
                else {
                    val replacement = Files.writeString(directory.resolve("replacement"), "foreign replacement")
                    Files.move(replacement, file, java.nio.file.StandardCopyOption.REPLACE_EXISTING)
                }
                val key = Files.readAttributes(marker, java.nio.file.attribute.BasicFileAttributes::class.java).fileKey()
                val before = Files.readAllBytes(marker)
                LocalCaptureOwnership.recover(namespace)
                assertArrayEquals(before, Files.readAllBytes(marker))
                assertEquals(key, Files.readAttributes(marker, java.nio.file.attribute.BasicFileAttributes::class.java).fileKey())
                assertTrue(Files.exists(old.root.resolve("scrcpy-server-v4.0")))
                assertTrue(Files.exists(old.descriptor))
            }
        }
    }

    @Test
    fun changedRootAndSymlinkedRootRingPacketOrDescriptorNeverFollowForeignData() {
        for (kind in listOf("changed-root", "changed-descriptor", "root", "ring", "packet", "descriptor")) {
            val namespace = namespace(kind)
            fixture(namespace).use { old ->
                val target = when (kind) {
                    "root", "changed-root" -> old.root
                    "ring" -> old.root.resolve("ring")
                    "packet" -> packet(old.root)
                    else -> old.descriptor
                }
                val retained = directory.resolve("retained-$kind")
                Files.move(target, retained)
                if (kind == "changed-root") Files.createDirectory(target)
                else if (kind == "changed-descriptor") Files.copy(retained, target)
                else Files.createSymbolicLink(target, retained)
                val bytes = if (Files.isRegularFile(retained)) Files.readAllBytes(retained) else null
                LocalCaptureOwnership.recover(namespace)
                assertTrue(Files.exists(retained))
                if (bytes != null) assertArrayEquals(bytes, Files.readAllBytes(retained))
                if (!kind.startsWith("changed-")) assertTrue(Files.isSymbolicLink(target))
                assertTrue(Files.exists(old.descriptor))
            }
        }
    }

    @Test
    fun fullLiveAndPinnedPacketCapacityFitsBothManifestBounds() {
        val ownership = LocalCaptureOwnership.create(namespace("capacity"))
        try {
            val record = Files.newDirectoryStream(ownership.root.parent, "local-capture-*.json").use { it.single() }
            val json = JsonParser.parseString(Files.readString(record)).asJsonObject
            val identity = json.getAsJsonObject("entries")["."].asJsonArray.map { it.asString }
            val entries = linkedMapOf<String, List<String>>()
            for (name in listOf(".", "ring", "scrcpy-server-v4.0", "replay-clock.jar")) entries[name] = identity
            repeat(2 * ReplaySettings.MAX_VIDEO_PACKETS + 1) {
                entries["ring/gop-${UUID.randomUUID()}.h264"] = identity
            }
            val encoded = ownership.encode(entries)
            assertEquals(LocalCaptureOwnership.MAX_ENTRIES, entries.size)
            assertTrue(encoded.size > 1024 * 1024) // The previous byte cap fails well before the supported packet cap.
            assertTrue(encoded.size <= LocalCaptureOwnership.MAX_BYTES)
            // Every identity is bounded at registration; 64 bytes covers the longest allowed name/punctuation.
            assertTrue(LocalCaptureOwnership.MAX_BYTES >= entries.size * (1024 + 64) + 2 * 1024 + 8192)
            entries["ring/gop-${UUID.randomUUID()}.h264"] = identity
            assertThrows(IllegalArgumentException::class.java) { ownership.encode(entries) }
        } finally { ownership.removeRoot(); ownership.close() }
    }

    @Test
    fun unknownAndFrozenWindowsRetireOnlyUnreferencedFilesBeforeRegistration() {
        for (frozen in listOf(false, true)) {
            val ownership = LocalCaptureOwnership.create(namespace("retention-$frozen"))
            val store = CaptureStore(ownership.root.resolve("ring"), videoLimit = 2, minFree = 0, created = ownership::created)
            try {
                store.generation(1)
                store.session(VideoPacket.Session(32, 32), 1)
                store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
                store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(1)), 1)
                val pinned = store.capture(ReplaySettings())!!
                assertEquals(Long.MAX_VALUE, pinned.endUncertainty)
                if (frozen) store.freeze()
                repeat(20) { store.frame(VideoPacket.Frame(200L + it, false, true, byteArrayOf(2)), 1); store.prune(900) }
                assertArrayEquals(byteArrayOf(1), Files.readAllBytes(pinned.video.single().file))
                assertEquals(if (frozen) 4 else 3, Files.list(store.directory).use { it.count() }.toInt()) // Frozen pruning leaves one just-evicted GOP until the next key.
                store.release(pinned.id)
                assertFalse(Files.exists(pinned.video.single().file))
                assertEquals(2, Files.list(store.directory).use { it.count() }.toInt())
            } finally { store.close(); ownership.removeRoot(); ownership.close() }
        }
    }

    @Test
    fun failedRegistrationLeavesNoPayloadOrLeakedCurrentChannelAndNextGopWorks() {
        val ring = directory.resolve("registration")
        var fail = true
        val store = CaptureStore(ring, minFree = 0, created = { path ->
            if (path != ring && fail) throw java.io.IOException("registration failed")
        })
        try {
            store.generation(1)
            store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            assertThrows(java.io.IOException::class.java) { store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(9)), 1) }
            assertEquals(0, Files.list(ring).use { it.count() })
            assertFalse(store.frame(VideoPacket.Frame(200, false, false, byteArrayOf(8)), 1))
            fail = false
            assertTrue(store.frame(VideoPacket.Frame(300, false, true, byteArrayOf(7)), 1))
            assertArrayEquals(byteArrayOf(7), Files.readAllBytes(Files.list(ring).use { it.findFirst().orElseThrow() }))
        } finally { store.close() }
        val root = namespace("resources-registration")
        assertThrows(java.io.IOException::class.java) { CaptureResources(root) { throw java.io.IOException("registration failed") } }
        assertEquals(0, Files.list(root).use { it.count() })
    }

    @Test
    fun sameDifferentAndUnavailableProcessStartHaveConservativeDecisions() {
        val recorded = Instant.parse("2020-01-01T00:00:00Z")
        assertFalse(LocalCaptureOwnership.ownerStopped(recorded, true, recorded))
        assertFalse(LocalCaptureOwnership.ownerStopped(recorded, true, null))
        assertTrue(LocalCaptureOwnership.ownerStopped(recorded, true, recorded.plusSeconds(1)))
        assertTrue(LocalCaptureOwnership.ownerStopped(recorded, false, null))
        fixture(namespace("reused-pid")).use { old ->
            val json = JsonParser.parseString(Files.readString(old.descriptor)).asJsonObject
            json.addProperty("pid", ProcessHandle.current().pid())
            json.addProperty("started", ProcessHandle.current().info().startInstant().orElseThrow().minusSeconds(1).toString())
            Files.writeString(old.descriptor, com.google.gson.Gson().toJson(json))
            LocalCaptureOwnership.recover(old.root.parent)
            assertFalse(Files.exists(old.root))
            assertFalse(Files.exists(old.descriptor))
        }
    }

    @Test
    fun childModesArePrivateBeforeRegistrationAndRootRemovalIsRetryable() {
        val ownership = LocalCaptureOwnership.create(namespace("permissions"))
        val created: (Path) -> Unit = { path ->
            val expected = if (Files.isDirectory(path)) "rwx------" else "rw-------"
            assertEquals(PosixFilePermissions.fromString(expected), Files.getPosixFilePermissions(path))
            ownership.created(path)
        }
        val resources = CaptureResources(ownership.root, created)
        val store = CaptureStore(ownership.root.resolve("ring"), minFree = 0, created = created)
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            assertTrue(store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1))
        } finally { store.close(); resources.close(); ownership.removeRoot(); ownership.removeRoot(); ownership.close(); ownership.close() }
    }

    @Test
    fun channelStillClosesWhenLockReleaseFails() {
        val ownership = LocalCaptureOwnership.create(namespace("release-failure"))
        val channelField = LocalCaptureOwnership::class.java.getDeclaredField("channel").apply { isAccessible = true }
        val channel = channelField.get(ownership) as FileChannel
        val lockField = LocalCaptureOwnership::class.java.getDeclaredField("lock").apply { isAccessible = true }
        lockField.set(ownership, object : FileLock(channel, 0, Long.MAX_VALUE, false) {
            override fun isValid() = true
            override fun release() { throw java.io.IOException("release failed") }
        })
        ownership.removeRoot()
        assertThrows(java.io.IOException::class.java) { ownership.close() }
        assertFalse(channel.isOpen)
    }

    @Test
    fun descriptorCleanupFailureReportsFailureButAllowsSubsequentDisableAndEnable() {
        val workspace = namespace("descriptor-failure")
        val core = ReplayCore(ReplaySettings(), workspace)
        try {
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            val descriptor = Files.newDirectoryStream(workspace, "local-capture-*.json").use { it.single() }
            val json = JsonParser.parseString(Files.readString(descriptor)).asJsonObject
            val root = workspace.resolve("replay-${json["token"].asString}")
            Files.setPosixFilePermissions(descriptor, PosixFilePermissions.fromString("rw-rw----"))
            val before = Files.readAllBytes(descriptor)
            assertFalse(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertFalse(core.snapshot().enabled)
            assertEquals(CaptureState.DISABLED, core.snapshot().captureState)
            assertFalse(Files.exists(root))
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(core.snapshot().enabled)
            assertArrayEquals(before, Files.readAllBytes(descriptor))
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
        } finally { core.closeAsync().get(10, TimeUnit.SECONDS) }
    }

    @Test
    fun unknownRootChildStaysIntactAndCleanupCanRetryAfterItsOwnerRemovesIt() {
        val workspace = namespace("root-retry")
        val core = ReplayCore(ReplaySettings(), workspace)
        try {
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            val root = Files.newDirectoryStream(workspace, "replay-*").use { it.single() }
            val foreign = Files.writeString(root.resolve("foreign.txt"), "owned fixture marker")
            assertFalse(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertTrue(core.snapshot().enabled)
            assertEquals("owned fixture marker", Files.readString(foreign))
            Files.delete(foreign)
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertFalse(core.snapshot().enabled)
            assertFalse(Files.exists(root))
        } finally { core.closeAsync().get(10, TimeUnit.SECONDS) }
    }

    @Test
    fun disableKeepsPinnedFilesAndEnabledStateUntilTheRealReaderFinishes() {
        val workspace = namespace("reader-order")
        val core = ReplayCore(ReplaySettings(), workspace)
        val release = java.util.concurrent.CountDownLatch(1)
        val reading = java.util.concurrent.CountDownLatch(1)
        var reader: Thread? = null
        try {
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            val field = ReplayCore::class.java.getDeclaredField("store").apply { isAccessible = true }
            val store = field.get(core) as CaptureStore
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1)
            val pinned = store.capture(ReplaySettings())!!
            val file = pinned.video.single().file
            val task = java.util.concurrent.FutureTask<Unit> {
                Files.newInputStream(file).use { input ->
                    assertEquals(2, input.read()); reading.countDown()
                    assertTrue(release.await(30, TimeUnit.SECONDS))
                }
            }
            reader = Thread(task, "owned-fixture-reader").also { it.start() }
            assertTrue(reading.await(10, TimeUnit.SECONDS))
            ReplayCore::class.java.getDeclaredField("saveTask").apply { isAccessible = true }.set(core, task)
            assertFalse(core.setEnabled(false).get(15, TimeUnit.SECONDS).accepted)
            assertTrue(core.snapshot().enabled)
            assertTrue(Files.exists(file))
            assertArrayEquals(byteArrayOf(2), Files.readAllBytes(file))
            release.countDown(); task.get(10, TimeUnit.SECONDS)
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertFalse(core.snapshot().enabled)
            assertFalse(Files.exists(file))
        } finally { release.countDown(); reader?.join(10_000); core.closeAsync().get(10, TimeUnit.SECONDS) }
    }

    @Test
    fun ordinaryCoreDisableRemovesOnlyItsLocalRecordAndRoot() {
        val workspace = namespace("ordinary")
        val output = Files.writeString(workspace.resolve("completed-output.txt"), "saved")
        val core = ReplayCore(ReplaySettings(), workspace)
        try {
            assertTrue(core.setEnabled(true).get(10, TimeUnit.SECONDS).accepted)
            val descriptor = Files.newDirectoryStream(workspace, "local-capture-*.json").use { it.single() }
            val json = JsonParser.parseString(Files.readString(descriptor)).asJsonObject
            assertEquals(setOf(".", "ring", "scrcpy-server-v4.0", "replay-clock.jar"), json.getAsJsonObject("entries").keySet())
            LocalCaptureOwnership.recover(workspace)
            assertTrue(Files.exists(descriptor))
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertFalse(Files.exists(descriptor))
            assertEquals(listOf(output), Files.list(workspace).use { it.toList() })
        } finally { core.closeAsync().get(10, TimeUnit.SECONDS) }
    }
}

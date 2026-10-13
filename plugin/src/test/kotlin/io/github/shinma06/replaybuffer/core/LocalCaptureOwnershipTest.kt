package io.github.shinma06.replaybuffer.core

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
import java.nio.ByteBuffer
import java.nio.file.StandardOpenOption.WRITE
import java.util.zip.CRC32
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
        val store = CaptureStore(ownership.root.resolve("ring"), minFree = 0,
            videoLimit = if (args[1] == "cycled") 2 else ReplaySettings.VIDEO_BYTES,
            created = ownership::created, removed = ownership::removed)
        store.generation(1)
        store.session(VideoPacket.Session(32, 32), 1)
        store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
        check(store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1))
        if (args[1] == "cycled") {
            store.capture(ReplaySettings())!!
            store.freeze()
            repeat(32) { store.frame(VideoPacket.Frame(200L + it, false, true, byteArrayOf(3)), 1) }
        }
        if (args[1] == "unlocked") ownership.close()
        println(ownership.root.fileName)
        System.out.flush()
        if (args[1] !in listOf("stopped", "cycled")) System.`in`.read()
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
            if (mode in listOf("stopped", "cycled")) {
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

    private fun header(path: Path) = LocalCaptureOwnership.decodeRecord(
        Files.newInputStream(path).use { it.readNBytes(LocalCaptureOwnership.HEADER_BYTES) }).asJsonObject

    private fun entries(path: Path): Map<String, List<String>> {
        val bytes = Files.readAllBytes(path)
        return (LocalCaptureOwnership.HEADER_BYTES until bytes.size step LocalCaptureOwnership.SLOT_BYTES).associate { offset ->
            val entry = LocalCaptureOwnership.decodeRecord(bytes.copyOfRange(offset, offset + LocalCaptureOwnership.SLOT_BYTES)).asJsonArray
            entry[0].asString to entry[1].asJsonArray.map { it.asString }
        }
    }

    private fun overwrite(path: Path, offset: Long, bytes: ByteArray) {
        FileChannel.open(path, WRITE).use { channel ->
            channel.position(offset)
            val buffer = ByteBuffer.wrap(bytes)
            while (buffer.hasRemaining()) channel.write(buffer)
            channel.force(true)
        }
    }

    private fun rawRecord(text: String, size: Int): ByteArray {
        val bytes = text.toByteArray(Charsets.UTF_8)
        return ByteBuffer.allocate(size).putInt(bytes.size).putInt(CRC32().apply { update(bytes) }.value.toInt()).put(bytes).array()
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
                else if (kind == "duplicate") overwrite(old.descriptor, 0,
                    rawRecord(com.google.gson.Gson().toJson(header(old.descriptor)).replaceFirst("{", "{\"schema\":2,"), LocalCaptureOwnership.HEADER_BYTES))
                else {
                    val json = header(old.descriptor)
                    when (kind) {
                        "pid", "started" -> json.remove(kind)
                        "namespace" -> json.addProperty(kind, "different project")
                        else -> {
                            overwrite(old.descriptor, LocalCaptureOwnership.HEADER_BYTES.toLong(),
                                LocalCaptureOwnership.encodeRecord(listOf("ring/gop-${UUID.randomUUID()}.h264", entries(old.descriptor).getValue(".")), LocalCaptureOwnership.SLOT_BYTES))
                        }
                    }
                    overwrite(old.descriptor, 0, LocalCaptureOwnership.encodeRecord(json, LocalCaptureOwnership.HEADER_BYTES))
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
    fun fixedSlotLayoutCoversFullCapacityAndLargestRegisteredIdentityWithinTheByteBound() {
        val ownership = LocalCaptureOwnership.create(namespace("capacity"))
        try {
            val record = Files.newDirectoryStream(ownership.root.parent, "local-capture-*.json").use { it.single() }
            val identity = entries(record).getValue(".").toMutableList()
            identity[0] += "x".repeat(1024 - com.google.gson.Gson().toJson(identity).toByteArray(Charsets.UTF_8).size)
            assertEquals(1024, com.google.gson.Gson().toJson(identity).toByteArray(Charsets.UTF_8).size)
            val name = "ring/gop-${UUID.randomUUID()}.h264"
            val slot = LocalCaptureOwnership.encodeRecord(listOf(name, identity), LocalCaptureOwnership.SLOT_BYTES)
            assertEquals(identity, LocalCaptureOwnership.decodeRecord(slot).asJsonArray[1].asJsonArray.map { it.asString })
            assertEquals(120005, LocalCaptureOwnership.MAX_ENTRIES)
            assertTrue(LocalCaptureOwnership.HEADER_BYTES.toLong() + LocalCaptureOwnership.MAX_ENTRIES.toLong() * slot.size <= LocalCaptureOwnership.MAX_BYTES)
            assertThrows(IllegalArgumentException::class.java) { LocalCaptureOwnership.encodeRecord(listOf(name, listOf("x".repeat(slot.size))), slot.size) }
        } finally { ownership.removeRoot(); ownership.close() }
    }

    @Test
    fun successiveRetainedGopsAppendOneFixedRecordBeforePayloadWithoutRewritingHistory() {
        for (frozen in listOf(false, true)) {
            val ownership = LocalCaptureOwnership.create(namespace("append-$frozen"))
            val descriptor = Files.newDirectoryStream(ownership.root.parent, "local-capture-*.json").use { it.single() }
            var registrations = 0
            var writtenBytes = 0L
            val deleted = mutableListOf<Path>()
            val store = CaptureStore(ownership.root.resolve("ring"), minFree = 0, created = { path ->
                val before = Files.readAllBytes(descriptor)
                ownership.created(path)
                val after = Files.readAllBytes(descriptor)
                assertEquals(LocalCaptureOwnership.SLOT_BYTES, after.size - before.size)
                assertArrayEquals(before, after.copyOf(before.size))
                writtenBytes += after.size - before.size
                if (Files.isRegularFile(path)) {
                    registrations++
                    assertEquals(0L, Files.size(path))
                    assertTrue("ring/${path.fileName}" in entries(descriptor))
                }
            }, removed = { path -> deleted.add(path); ownership.removed(path) })
            try {
                store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
                store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
                store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1)
                val pin = store.capture(ReplaySettings())!!
                assertEquals(Long.MAX_VALUE, pin.endUncertainty)
                if (frozen) store.freeze()
                repeat(64) { assertTrue(store.frame(VideoPacket.Frame(200L + it, false, true, byteArrayOf(3)), 1)) }
                assertEquals(65, registrations)
                assertEquals(66L * LocalCaptureOwnership.SLOT_BYTES, writtenBytes) // Ring plus65 new GOPs.
                assertTrue(deleted.isEmpty())
                assertArrayEquals(byteArrayOf(2), Files.readAllBytes(pin.video.single().file))
            } finally { store.close(); ownership.removeRoot(); ownership.close() }
        }
    }

    @Test
    fun exactUnlinksRecycleSlotsAndBoundPersistentGrowthWhilePinAndLiveDataRemain() {
        for (frozen in listOf(false, true)) {
            val ownership = LocalCaptureOwnership.create(namespace("reuse-$frozen"))
            val descriptor = Files.newDirectoryStream(ownership.root.parent, "local-capture-*.json").use { it.single() }
            val deleted = mutableListOf<Path>()
            val store = CaptureStore(ownership.root.resolve("ring"), videoLimit = 2, minFree = 0,
                created = ownership::created, removed = { path ->
                    assertFalse(Files.exists(path))
                    assertFalse(path in deleted)
                    deleted.add(path)
                    ownership.removed(path)
                })
            try {
                store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
                store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
                store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1)
                val pin = store.capture(ReplaySettings())!!
                if (frozen) store.freeze()
                repeat(48) {
                    assertTrue(store.frame(VideoPacket.Frame(200L + it, false, true, byteArrayOf(3)), 1))
                    assertTrue(Files.size(descriptor) <= LocalCaptureOwnership.HEADER_BYTES + 6L * LocalCaptureOwnership.SLOT_BYTES)
                    assertTrue(Files.exists(pin.video.single().file))
                }
                assertTrue(deleted.isNotEmpty())
                assertEquals(deleted.distinct(), deleted)
                val beforeRelease = Files.readAllBytes(descriptor)
                store.release(pin.id)
                assertFalse(Files.exists(pin.video.single().file))
                assertArrayEquals(beforeRelease, Files.readAllBytes(descriptor)) // Unlink needs no full journal update.
                assertTrue(store.frame(VideoPacket.Frame(300, false, true, byteArrayOf(4)), 1))
                val after = Files.readAllBytes(descriptor)
                assertEquals(beforeRelease.size, after.size)
                assertArrayEquals(beforeRelease.copyOf(LocalCaptureOwnership.HEADER_BYTES), after.copyOf(LocalCaptureOwnership.HEADER_BYTES))
                val changedSlots = (LocalCaptureOwnership.HEADER_BYTES until after.size step LocalCaptureOwnership.SLOT_BYTES).count { offset ->
                    !beforeRelease.copyOfRange(offset, offset + LocalCaptureOwnership.SLOT_BYTES)
                        .contentEquals(after.copyOfRange(offset, offset + LocalCaptureOwnership.SLOT_BYTES))
                }
                assertEquals(1, changedSlots)
            } finally { store.close(); ownership.removeRoot(); ownership.close() }
        }
    }

    @Test
    fun stoppedOwnerWithReusedSlotsAndMissingOldEntriesIsReclaimed() {
        fixture(namespace("cycled"), "cycled").use { old ->
            assertTrue(Files.size(old.descriptor) <= LocalCaptureOwnership.HEADER_BYTES + 8L * LocalCaptureOwnership.SLOT_BYTES)
            assertArrayEquals(byteArrayOf(2), Files.readAllBytes(old.root.resolve(entries(old.descriptor).keys.first {
                it.startsWith("ring/") && Files.exists(old.root.resolve(it)) && Files.readAllBytes(old.root.resolve(it)).contentEquals(byteArrayOf(2))
            })))
            LocalCaptureOwnership.recover(old.root.parent)
            assertFalse(Files.exists(old.root))
            assertFalse(Files.exists(old.descriptor))
        }
    }

    @Test
    fun incompleteGrowthReusedSlotBadPaddingAndDuplicateEntriesPreserveEveryFile() {
        for (kind in listOf("growth", "reused", "padding", "duplicate")) {
            fixture(namespace("incomplete-$kind"), "cycled").use { old ->
                val files = Files.walk(old.root).use { stream -> stream.filter { Files.isRegularFile(it) }.toList() }
                val before = files.associateWith { Files.readAllBytes(it) }
                val bytes = Files.readAllBytes(old.descriptor)
                when (kind) {
                    "growth" -> FileChannel.open(old.descriptor, WRITE).use { it.truncate(bytes.size.toLong() - 1) }
                    "reused" -> {
                        val index = LocalCaptureOwnership.HEADER_BYTES + 5 * LocalCaptureOwnership.SLOT_BYTES + 10
                        bytes[index] = (bytes[index].toInt() xor 1).toByte()
                        Files.write(old.descriptor, bytes)
                    }
                    "padding" -> { bytes[bytes.lastIndex] = 1; Files.write(old.descriptor, bytes) }
                    "duplicate" -> overwrite(old.descriptor, bytes.size.toLong(), bytes.copyOfRange(LocalCaptureOwnership.HEADER_BYTES, LocalCaptureOwnership.HEADER_BYTES + LocalCaptureOwnership.SLOT_BYTES))
                }
                LocalCaptureOwnership.recover(old.root.parent)
                before.forEach { (path, payload) -> assertArrayEquals(payload, Files.readAllBytes(path)) }
                assertTrue(Files.exists(old.descriptor))
            }
        }
    }

    @Test
    fun validLegacyWholeJsonRecordsAreConservativelyRetained() {
        fixture(namespace("legacy")).use { old ->
            val json = header(old.descriptor)
            json.addProperty("schema", 1)
            json.add("entries", com.google.gson.Gson().toJsonTree(entries(old.descriptor)))
            Files.writeString(old.descriptor, com.google.gson.Gson().toJson(json))
            val before = Files.readAllBytes(packet(old.root))
            LocalCaptureOwnership.recover(old.root.parent)
            assertArrayEquals(before, Files.readAllBytes(packet(old.root)))
            assertTrue(Files.exists(old.descriptor))
        }
    }

    @Test
    fun failedPersistenceRejectsFurtherRegistrationAndWritesNoGopPayload() {
        val ownership = LocalCaptureOwnership.create(namespace("persistence-failure"))
        val store = CaptureStore(ownership.root.resolve("ring"), minFree = 0,
            created = ownership::created, removed = ownership::removed)
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            val channel = LocalCaptureOwnership::class.java.getDeclaredField("channel").apply { isAccessible = true }.get(ownership) as FileChannel
            channel.close() // Deterministic ordinary IO failure; no concurrent interleaving.
            assertThrows(java.nio.channels.ClosedChannelException::class.java) { store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(9)), 1) }
            assertThrows(IllegalStateException::class.java) { store.frame(VideoPacket.Frame(200, false, true, byteArrayOf(8)), 1) }
            assertEquals(0L, Files.list(store.directory).use { it.count() })
        } finally { store.close(); ownership.removeRoot(); ownership.close() }
    }

    @Test
    fun failedUnlinkPreservesPayloadAndDoesNotNotifyOwnershipUntilDeletionSucceeds() {
        val ownership = LocalCaptureOwnership.create(namespace("unlink-failure"))
        val deleted = mutableListOf<Path>()
        val store = CaptureStore(ownership.root.resolve("ring"), videoLimit = 1, minFree = 0,
            created = ownership::created, removed = { path -> deleted.add(path); ownership.removed(path) })
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1)
            val pin = store.capture(ReplaySettings())!!
            store.frame(VideoPacket.Frame(200, false, true, byteArrayOf(3)), 1)
            Files.setPosixFilePermissions(store.directory, PosixFilePermissions.fromString("r-x------"))
            try {
                assertThrows(java.io.IOException::class.java) { store.release(pin.id) }
                assertArrayEquals(byteArrayOf(2), Files.readAllBytes(pin.video.single().file))
                assertTrue(deleted.isEmpty())
            } finally { Files.setPosixFilePermissions(store.directory, PosixFilePermissions.fromString("rwx------")) }
            store.prune(900)
            assertEquals(listOf(pin.video.single().file), deleted)
            assertFalse(Files.exists(pin.video.single().file))
        } finally { store.close(); ownership.removeRoot(); ownership.close() }
    }

    @Test
    fun unlinkNotificationFailureIsNotRepeatedForAnAlreadyDeletedFile() {
        val ring = directory.resolve("notification-failure")
        val removed = mutableListOf<Path>()
        var fail = true
        val store = CaptureStore(ring, videoLimit = 1, minFree = 0, removed = { path ->
            assertFalse(Files.exists(path))
            removed.add(path)
            if (fail) throw java.io.IOException("notification failed")
        })
        try {
            store.generation(1); store.session(VideoPacket.Session(32, 32), 1)
            store.frame(VideoPacket.Frame(0, true, false, byteArrayOf(1)), 1)
            store.frame(VideoPacket.Frame(100, false, true, byteArrayOf(2)), 1)
            val pin = store.capture(ReplaySettings())!!
            store.frame(VideoPacket.Frame(200, false, true, byteArrayOf(3)), 1)
            assertThrows(java.io.IOException::class.java) { store.release(pin.id) }
            assertFalse(Files.exists(pin.video.single().file))
            assertEquals(listOf(pin.video.single().file), removed)
            fail = false
            store.frame(VideoPacket.Frame(300, false, true, byteArrayOf(4)), 1)
        } finally { store.close() }
        assertEquals(removed.distinct(), removed)
    }

    @Test
    fun unknownAndFrozenWindowsRetireOnlyUnreferencedFilesBeforeRegistration() {
        for (frozen in listOf(false, true)) {
            val ownership = LocalCaptureOwnership.create(namespace("retention-$frozen"))
            val store = CaptureStore(ownership.root.resolve("ring"), videoLimit = 2, minFree = 0, created = ownership::created, removed = ownership::removed)
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
            val json = header(old.descriptor)
            json.addProperty("pid", ProcessHandle.current().pid())
            json.addProperty("started", ProcessHandle.current().info().startInstant().orElseThrow().minusSeconds(1).toString())
            overwrite(old.descriptor, 0, LocalCaptureOwnership.encodeRecord(json, LocalCaptureOwnership.HEADER_BYTES))
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
        val store = CaptureStore(ownership.root.resolve("ring"), minFree = 0, created = created, removed = ownership::removed)
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
            val json = header(descriptor)
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
            val json = header(descriptor)
            assertEquals(setOf(".", "ring", "scrcpy-server-v4.0", "replay-clock.jar"), entries(descriptor).keys)
            LocalCaptureOwnership.recover(workspace)
            assertTrue(Files.exists(descriptor))
            assertTrue(core.setEnabled(false).get(10, TimeUnit.SECONDS).accepted)
            assertFalse(Files.exists(descriptor))
            assertEquals(listOf(output), Files.list(workspace).use { it.toList() })
        } finally { core.closeAsync().get(10, TimeUnit.SECONDS) }
    }
}

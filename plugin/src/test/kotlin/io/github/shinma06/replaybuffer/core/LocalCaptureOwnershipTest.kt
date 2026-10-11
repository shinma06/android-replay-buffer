package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
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
                if (!process.waitFor(10, TimeUnit.SECONDS)) {
                    process.destroyForcibly()
                    assertTrue(process.waitFor(10, TimeUnit.SECONDS))
                }
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
            process.destroyForcibly()
            process.waitFor(10, TimeUnit.SECONDS)
            throw failure
        }
    }

    private fun packet(root: Path) = Files.list(root.resolve("ring")).use { it.findFirst().orElseThrow() }
    private fun namespace(name: String) = Files.createDirectory(directory.resolve(name))

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

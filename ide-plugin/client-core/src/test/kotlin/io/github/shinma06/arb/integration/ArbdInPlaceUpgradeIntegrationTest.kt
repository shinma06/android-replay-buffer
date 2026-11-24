package io.github.shinma06.arb.integration

import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.HostOs
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.SystemHelloResponse
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.FileTime
import java.time.Duration
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * G-M9 U3 / U4 (plugin side): an `arbd` of an older plugin build keeps running while the plugin ZIP is
 * overwritten with a new build. Entries of the release ZIP carry one fixed timestamp, so size + mtime
 * cannot tell two builds apart; the `build_id` content digest must, and the replacement must be
 * `system.shutdown` of the old daemon followed by a start of the new one, with `config.json` intact.
 */
class ArbdInPlaceUpgradeIntegrationTest {
    private fun repoRoot(): Path {
        val prop = System.getProperty("arb.repo.root")
        if (!prop.isNullOrBlank()) return Paths.get(prop).toAbsolutePath().normalize()
        var cur: Path? = Paths.get("").toAbsolutePath().normalize()
        repeat(6) {
            val d = cur ?: error("Could not locate repository root")
            if (Files.isDirectory(d.resolve("core")) && Files.isDirectory(d.resolve("fixtures"))) return d
            cur = d.parent
        }
        error("Could not locate repository root containing core/ and fixtures/")
    }

    private fun sha256(bytes: ByteArray): String =
        java.util.HexFormat.of().formatHex(java.security.MessageDigest.getInstance("SHA-256").digest(bytes))

    private fun zipOf(fixedTime: Long, entries: Map<String, ByteArray>): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        ZipOutputStream(out).use { zip ->
            for ((name, data) in entries) {
                val entry = ZipEntry(name)
                entry.time = fixedTime
                zip.putNextEntry(entry)
                zip.write(data)
                zip.closeEntry()
            }
        }
        return out.toByteArray()
    }

    private fun extractOver(zipBytes: ByteArray, dest: Path) {
        ZipInputStream(zipBytes.inputStream()).use { zip ->
            var entry = zip.nextEntry
            while (entry != null) {
                val target = dest.resolve(entry.name)
                Files.createDirectories(target.parent)
                Files.write(target, zip.readAllBytes())
                Files.setLastModifiedTime(target, FileTime.fromMillis(entry.time))
                entry = zip.nextEntry
            }
        }
    }

    private fun hello(buildId: String, mtime: Long) = SystemHelloResponse(
        protocolVersion = 1,
        schemaVersion = 2,
        pid = 1,
        dataDir = "",
        cacheDir = "",
        runtimeDir = "",
        daemonVersion = "2.0.0",
        buildId = buildId,
        binaryMtimeSecs = mtime,
    )

    @Test
    fun overwritingTheBundledArbdInPlaceWithFixedZipTimestampsIsDetectedByContentDigestExactlyOnce(@TempDir tmp: Path) {
        val fixed = 1_795_392_000_000L
        val oldBytes = "arbd-build-AAAA-const=1".toByteArray()
        val newBytes = "arbd-build-AAAA-const=2".toByteArray()
        assertEquals(oldBytes.size, newBytes.size)
        val pluginDir = tmp.resolve("plugins").resolve("arb-ide-plugin")
        val rel = "native/linux-x86_64/arbd"

        extractOver(zipOf(fixed, mapOf(rel to oldBytes)), pluginDir)
        val bin = pluginDir.resolve(rel)
        val runningOld = hello("2.0.0-${oldBytes.size}-${fixed / 1000}:sha256:${sha256(oldBytes)}", fixed / 1000)

        // The IDE keeps the old daemon running, then the new plugin ZIP is extracted over the same path.
        extractOver(zipOf(fixed, mapOf(rel to newBytes)), pluginDir)
        assertEquals(FileTime.fromMillis(fixed), Files.getLastModifiedTime(bin), "fixed ZIP mtime survives extraction")
        assertEquals(oldBytes.size.toLong(), Files.size(bin), "same size as the build that is still running")

        val paths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"), os = HostOs.LINUX)
        val manager = ArbdProcessManager(paths = paths, os = HostOs.LINUX, arch = "x64", parentBound = true)
        assertTrue(manager.hasPendingBundledUpgrade(bin, bin, runningOld), "upgrade the previous unbound plugin build")
        assertFalse(manager.hasPendingBundledUpgrade(bin, null, runningOld), "custom binary is outside the bundled upgrade path")
        assertFalse(manager.hasPendingBundledUpgrade(bin, bin, runningOld.copy(parentBound = true)), "respect another live IDE")
        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, fixed / 1000, "2.0.0-${oldBytes.size}-${fixed / 1000}"),
            "precondition: size + mtime alone cannot see the overwrite",
        )
        assertTrue(manager.shouldRestartForIdentity(bin, bin, runningOld), "the digest mismatch must trigger the restart")
        val runningNew = hello("2.0.0-${newBytes.size}-${fixed / 1000}:sha256:${sha256(newBytes)}", fixed / 1000)
        assertFalse(manager.shouldRestartForIdentity(bin, bin, runningNew), "the replacement daemon reports the new digest")
        assertFalse(manager.shouldRestartForIdentity(bin, bin, runningOld), "loop guard: at most one identity restart per session")
    }

    @Test
    fun restartDaemonShutsTheRunningArbdDownStartsTheNewOneAndKeepsConfig(@TempDir tmp: Path) {
        val root = repoRoot()
        val discovery = ArbdProcessManager(
            paths = ArbPaths(tmp.resolve("probe-d"), tmp.resolve("probe-c"), tmp.resolve("probe-r")),
            searchRoot = root,
        )
        val built = discovery.resolveDaemonBinary()
        assertNotNull(built, "arbd binary not found. Run `cargo build --manifest-path core/Cargo.toml -p arbd` first.")

        val oldBin = tmp.resolve("old-plugin").resolve("arbd" + if (HostOs.detect() == HostOs.WINDOWS) ".exe" else "")
        val newBin = tmp.resolve("new-plugin").resolve(oldBin.fileName.toString())
        for (dest in listOf(oldBin, newBin)) {
            Files.createDirectories(dest.parent)
            Files.copy(built, dest, StandardCopyOption.REPLACE_EXISTING)
            dest.toFile().setExecutable(true)
        }
        val paths = ArbPaths(dataDir = tmp.resolve("data"), cacheDir = tmp.resolve("cache"), runtimeDir = tmp.resolve("run"))
        fun managerFor(bin: Path) = ArbdProcessManager(
            paths = paths,
            searchRoot = null,
            env = { key -> if (key == "ARB_DAEMON_PATH") bin.toString() else null },
        )

        val first = managerFor(oldBin).connectOrSpawn(Duration.ofSeconds(15))
        val oldProcess = first.spawnedProcess
        assertNotNull(oldProcess, "the first connect must spawn the old arbd")
        val oldPid = first.hello.pid
        first.client.use { rpc ->
            val updated = rpc.configUpdate(ArbConfigPatch(saveWindowSecs = 77)).config
            assertEquals(77, updated.saveWindowSecs)
        }

        val second = managerFor(newBin).restartDaemon(Duration.ofSeconds(15))
        try {
            assertNotEquals(oldPid, second.hello.pid, "a new arbd process must be serving")
            assertTrue(oldProcess.waitFor(10, java.util.concurrent.TimeUnit.SECONDS), "the old arbd must have exited after system.shutdown")
            second.client.use { rpc ->
                assertEquals(77, rpc.configGet().config.saveWindowSecs, "config.json survives the restart")
                rpc.systemShutdown()
            }
        } finally {
            second.spawnedProcess?.let { if (it.isAlive && !it.waitFor(5, java.util.concurrent.TimeUnit.SECONDS)) it.destroyForcibly() }
        }
    }
}

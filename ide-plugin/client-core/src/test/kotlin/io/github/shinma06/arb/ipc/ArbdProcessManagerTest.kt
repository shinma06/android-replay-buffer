package io.github.shinma06.arb.ipc

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import io.github.shinma06.arb.model.SystemHelloResponse
import java.nio.file.attribute.FileTime
import java.security.MessageDigest
import java.time.Instant
import java.util.HexFormat
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ArbdProcessManagerTest {
    @Test
    fun resolvesBundledBinaryUnderGradleX8664Directory(@TempDir tmp: Path) {
        val pluginDir = tmp.resolve("plugin-root")
        val bundled = pluginDir.resolve("native").resolve("linux-x86_64").resolve("arbd")
        Files.createDirectories(bundled.parent)
        Files.writeString(bundled, "stub")
        val paths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"), os = HostOs.LINUX)
        val mgr = ArbdProcessManager(paths = paths, pluginDir = pluginDir, os = HostOs.LINUX, arch = "x64")
        assertEquals(bundled, mgr.resolveBundledBinary("arbd"))
    }

    @Test
    fun resolvesDaemonBinaryInPriorityOrder(@TempDir tmp: Path) {
        val envBin = Files.writeString(tmp.resolve("env-arbd"), "stub")
        val pluginDir = tmp.resolve("plugin-root")
        val bundledBin = pluginDir.resolve("bin").resolve("macos-arm64").resolve("arbd")
        Files.createDirectories(bundledBin.parent)
        Files.writeString(bundledBin, "stub")

        val devRoot = tmp.resolve("repo")
        val devBin = devRoot.resolve("core").resolve("target").resolve("debug").resolve("arbd")
        Files.createDirectories(devBin.parent)
        Files.writeString(devBin, "stub")

        val dummyPaths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"), os = HostOs.MACOS)

        // 1. ARB_DAEMON_PATH wins over custom override, bundled, and dev.
        val customBin = Files.writeString(tmp.resolve("custom-arbd"), "stub")
        val mgr1 = ArbdProcessManager(
            paths = dummyPaths,
            pluginDir = pluginDir,
            searchRoot = devRoot,
            os = HostOs.MACOS,
            arch = "arm64",
            env = mapOf(ArbdProcessManager.ENV_ARB_DAEMON_PATH to envBin.toString())::get,
            customBinaryProvider = { customBin },
        )
        assertEquals(envBin, mgr1.resolveDaemonBinary())

        // 2. Custom IDE Settings override wins over bundled, dev, and PATH when env unset.
        val mgrCustom = ArbdProcessManager(
            paths = dummyPaths,
            pluginDir = pluginDir,
            searchRoot = devRoot,
            os = HostOs.MACOS,
            arch = "arm64",
            env = { null },
            customBinaryProvider = { customBin },
        )
        assertEquals(customBin, mgrCustom.resolveDaemonBinary())

        // 3. Bundled bin/<os>-<arch>/arbd wins over dev path when env & custom override unset.
        val mgr2 = ArbdProcessManager(
            paths = dummyPaths,
            pluginDir = pluginDir,
            searchRoot = devRoot,
            os = HostOs.MACOS,
            arch = "arm64",
            env = { null },
        )
        assertEquals(bundledBin, mgr2.resolveDaemonBinary())

        // 3. Sibling core/target/debug/arbd used when neither env nor bundled exists.
        val mgr3 = ArbdProcessManager(
            paths = dummyPaths,
            pluginDir = null,
            searchRoot = devRoot.resolve("ide-plugin"),
            os = HostOs.MACOS,
            arch = "arm64",
            env = { null },
        )
        assertEquals(devBin, mgr3.resolveDaemonBinary())
    }

    @Test
    fun resolvesBundledNativeBinariesDeviceAgentJarsAndGuiAdbWithExecutableBit(@TempDir tmp: Path) {
        val pluginDir = tmp.resolve("arb-ide-plugin")
        val nativeDir = pluginDir.resolve("native").resolve("macos-arm64")
        val agentDir = pluginDir.resolve("device-agent")
        Files.createDirectories(nativeDir)
        Files.createDirectories(agentDir)

        val bundledArbd = Files.writeString(nativeDir.resolve("arbd"), "#!/bin/sh\n")
        val bundledTray = Files.writeString(nativeDir.resolve("arb-tray"), "#!/bin/sh\n")
        // Strip executable bits to prove resolveDaemonBinary/resolveTrayBinary restores +x on Unix
        bundledArbd.toFile().setExecutable(false, false)
        bundledTray.toFile().setExecutable(false, false)

        val compJar = Files.writeString(agentDir.resolve("arb-companion.dex.jar"), "jar")
        val scrcpyJar = Files.writeString(agentDir.resolve("replay-scrcpy-audio.dex.jar"), "jar")

        // Simulate macOS GUI launch with minimal PATH=/usr/bin:/bin and no ANDROID_HOME,
        // where adb lives at ~/Library/Android/sdk/platform-tools/adb
        val fakeHome = tmp.resolve("home")
        val fakeAdb = fakeHome.resolve("Library").resolve("Android").resolve("sdk").resolve("platform-tools").resolve("adb")
        Files.createDirectories(fakeAdb.parent)
        Files.writeString(fakeAdb, "#!/bin/sh\n")

        val mgr = ArbdProcessManager(
            paths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"), os = HostOs.MACOS),
            pluginDir = pluginDir,
            searchRoot = tmp.resolve("unrelated"),
            os = HostOs.MACOS,
            arch = "arm64",
            env = mapOf("PATH" to "/usr/bin:/bin", "HOME" to fakeHome.toString())::get,
        )

        val resolvedArbd = mgr.resolveDaemonBinary()
        val resolvedTray = mgr.resolveTrayBinary()
        assertEquals(bundledArbd, resolvedArbd)
        assertEquals(bundledTray, resolvedTray)
        if (HostOs.detect().usesExecuteBit) {
            assertTrue(Files.isExecutable(resolvedArbd!!), "bundled arbd must be marked executable on Unix")
            assertTrue(Files.isExecutable(resolvedTray!!), "bundled arb-tray must be marked executable on Unix")
        }
        assertEquals(compJar, mgr.resolveCompanionJar())
        assertEquals(scrcpyJar, mgr.resolveScrcpyJar())
        assertEquals(fakeAdb, mgr.resolveAdbBinary())

        val childEnv = mgr.buildDaemonEnvironment(resolvedArbd)
        assertEquals(compJar.toAbsolutePath().normalize().toString(), childEnv[ArbdProcessManager.ENV_ARB_COMPANION_JAR])
        assertEquals(scrcpyJar.toAbsolutePath().normalize().toString(), childEnv[ArbdProcessManager.ENV_ARB_SCRCPY_JAR])
        assertEquals(fakeAdb.toAbsolutePath().normalize().toString(), childEnv[ArbdProcessManager.ENV_ARB_ADB_PATH])
    }

    @Test
    fun autoUpgradeDetectsNewerBundledDaemonBinaryAndSkipsSameFile(@TempDir tmp: Path) {
        val oldBin = Files.writeString(tmp.resolve("old-arbd"), "v1")
        val newBin = Files.writeString(tmp.resolve("new-arbd"), "v2")
        Files.setLastModifiedTime(oldBin, FileTime.from(Instant.ofEpochSecond(1_700_000_000L)))
        Files.setLastModifiedTime(newBin, FileTime.from(Instant.ofEpochSecond(1_700_000_600L)))

        assertTrue(ArbdProcessManager.shouldAutoUpgradeBinary(oldBin, newBin), "newer bundled arbd must trigger upgrade")
        assertFalse(ArbdProcessManager.shouldAutoUpgradeBinary(newBin, oldBin), "older bundled arbd must not downgrade")
        assertFalse(ArbdProcessManager.shouldAutoUpgradeBinary(newBin, newBin), "identical binary must not restart")
    }

    @Test
    fun parsesSystemHelloBuildIdFromTheEnd() {
        assertEquals(
            DaemonBuildIdentity(version = "0.1.0", sizeBytes = 12_345L, mtimeSecs = 1_700_000_000L),
            ArbdProcessManager.parseBuildIdentity("0.1.0-12345-1700000000"),
        )
        // Pre-release versions contain '-' themselves.
        assertEquals(
            DaemonBuildIdentity(version = "2.0.0-dev.7", sizeBytes = 9L, mtimeSecs = 0L),
            ArbdProcessManager.parseBuildIdentity("2.0.0-dev.7-9-0"),
        )
        assertNull(ArbdProcessManager.parseBuildIdentity(null))
        assertNull(ArbdProcessManager.parseBuildIdentity(""))
        assertNull(ArbdProcessManager.parseBuildIdentity("0.1.0"))
        assertNull(ArbdProcessManager.parseBuildIdentity("0.1.0-abc-1"))
        assertNull(ArbdProcessManager.parseBuildIdentity("-12-34"))
    }

    @Test
    fun samePathUpgradeDetectsSizeChangeWhenZipTimestampsAreFixed(@TempDir tmp: Path) {
        val bin = tmp.resolve("arbd")
        Files.writeString(bin, "new-binary-bytes")
        val fixed = 315_532_800L // 1980-01-01: reproducible ZIP entry timestamp
        Files.setLastModifiedTime(bin, FileTime.from(Instant.ofEpochSecond(fixed)))
        val size = Files.size(bin)

        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, fixed, "0.1.0-$size-$fixed"),
            "identical identity must not restart",
        )
        assertTrue(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, fixed, "0.1.0-${size + 7}-$fixed"),
            "same path + same mtime but different size => plugin ZIP was replaced in place",
        )
        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, fixed, "garbage"),
            "unparseable build_id never forces a restart",
        )
    }

    @Test
    fun identityAutoUpgradeIsClaimedAtMostOncePerSession(@TempDir tmp: Path) {
        val paths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"), os = HostOs.LINUX)
        val mgr = ArbdProcessManager(paths = paths, os = HostOs.LINUX, arch = "x64")
        assertFalse(mgr.identityUpgradeAttempted)
        assertTrue(mgr.tryClaimIdentityUpgrade())
        assertTrue(mgr.identityUpgradeAttempted)
        assertFalse(mgr.tryClaimIdentityUpgrade(), "loop guard: a second identity restart is refused")
        assertFalse(mgr.tryClaimIdentityUpgrade())
    }

    private fun sha256(bytes: ByteArray): String =
        HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes))

    private fun hello(buildId: String, mtime: Long) = SystemHelloResponse(
        protocolVersion = 1,
        schemaVersion = 2,
        pid = 42L,
        dataDir = "/d",
        cacheDir = "/c",
        runtimeDir = "/r",
        daemonVersion = "2.0.0",
        buildId = buildId,
        binaryMtimeSecs = mtime,
    )

    @Test
    fun parsesContentDigestBuildIdWithAndWithoutLegacyPrefix() {
        val hex = "ab".repeat(32)
        assertEquals(
            DaemonBuildIdentity(version = "2.0.0", sha256Hex = hex),
            ArbdProcessManager.parseBuildIdentity("2.0.0:sha256:$hex"),
        )
        assertEquals(
            DaemonBuildIdentity(version = "2.1.0-rc.1", sizeBytes = 9L, mtimeSecs = 5L, sha256Hex = hex),
            ArbdProcessManager.parseBuildIdentity("2.1.0-rc.1-9-5:sha256:${hex.uppercase()}"),
        )
        assertNull(ArbdProcessManager.parseBuildIdentity("2.0.0:sha256:nothex"), "malformed digest")
        assertNull(ArbdProcessManager.parseBuildIdentity(":sha256:$hex"), "missing version")
    }

    /**
     * M9 fail-first: a reproducible plugin ZIP replaced in place yields the same path, the same size
     * and the same mtime when only constants/strings changed. The size/mtime identity cannot tell
     * the builds apart; the content digest must trigger exactly one restart.
     */
    @Test
    fun samePathSameSizeSameMtimeDifferentBytesTriggersExactlyOneRestart(@TempDir tmp: Path) {
        val fixed = 1_795_392_000L // ZIP_TIMESTAMP-style fixed entry time
        val oldBytes = "arbd-build-AAAA-const=1".toByteArray()
        val newBytes = "arbd-build-AAAA-const=2".toByteArray()
        assertEquals(oldBytes.size, newBytes.size, "fixture builds must have identical size")

        val bin = tmp.resolve("native").resolve("arbd")
        Files.createDirectories(bin.parent)
        Files.write(bin, newBytes) // the plugin ZIP overwrote the running daemon's path in place
        Files.setLastModifiedTime(bin, FileTime.from(Instant.ofEpochSecond(fixed)))
        val size = Files.size(bin)

        // The running daemon was started from the *old* bytes at the same path.
        val runningOld = hello("2.0.0-$size-$fixed:sha256:${sha256(oldBytes)}", fixed)
        val legacyOnly = "2.0.0-$size-$fixed"
        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, fixed, legacyOnly),
            "precondition: size/mtime identity alone cannot detect the replaced build",
        )

        val paths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"), os = HostOs.LINUX)
        val mgr = ArbdProcessManager(paths = paths, os = HostOs.LINUX, arch = "x64")
        var restarts = 0
        var running = runningOld
        repeat(4) {
            if (mgr.shouldRestartForIdentity(bin, bin, running)) {
                restarts += 1
                // The respawned daemon now reports the digest of the bundled (new) bytes.
                running = hello("2.0.0-$size-$fixed:sha256:${sha256(newBytes)}", fixed)
            }
        }
        assertEquals(1, restarts, "content-digest mismatch must restart exactly once")
        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, fixed, running.buildId),
            "matching digest must not restart again",
        )
    }

    @Test
    fun warmedDigestDetectsInPlaceOverwrite(@TempDir tmp: Path) {
        val bin = Files.writeString(tmp.resolve("arbd"), "old")
        val timestamp = Files.getLastModifiedTime(bin)
        val digest = ArbdProcessManager.sha256Hex(bin)
        Files.writeString(bin, "new")
        Files.setLastModifiedTime(bin, timestamp)
        assertTrue(ArbdProcessManager.shouldAutoUpgradeBinary(bin, bin, null, "1:sha256:$digest"))
    }

    @Test
    fun digestMismatchThatSurvivesRestartIsStillLimitedToOneRestart(@TempDir tmp: Path) {
        val bin = Files.writeString(tmp.resolve("arbd"), "bundled-bytes")
        val stale = hello("2.0.0:sha256:${sha256("stale-bytes".toByteArray())}", 0L)
        val paths = ArbPaths(tmp.resolve("d"), tmp.resolve("c"), tmp.resolve("r"), os = HostOs.LINUX)
        val mgr = ArbdProcessManager(paths = paths, os = HostOs.LINUX, arch = "x64")
        val restarts = (1..5).count { mgr.shouldRestartForIdentity(bin, bin, stale) }
        assertEquals(1, restarts, "an unkillable stale daemon must not cause a restart loop")
    }

    @Test
    fun differentPathWithIdenticalDigestNeverRestartsEvenIfBundledIsNewer(@TempDir tmp: Path) {
        val running = Files.writeString(tmp.resolve("running-arbd"), "same-bytes")
        val bundled = Files.writeString(tmp.resolve("bundled-arbd"), "same-bytes")
        Files.setLastModifiedTime(running, FileTime.from(Instant.ofEpochSecond(1_700_000_000L)))
        Files.setLastModifiedTime(bundled, FileTime.from(Instant.ofEpochSecond(1_700_000_600L)))
        val digest = sha256("same-bytes".toByteArray())
        assertFalse(
            ArbdProcessManager.shouldAutoUpgradeBinary(running, bundled, null, "2.0.0:sha256:$digest"),
            "byte-identical bundled arbd must not restart the daemon",
        )
        assertTrue(
            ArbdProcessManager.shouldAutoUpgradeBinary(running, bundled, null, "2.0.0:sha256:${"0".repeat(64)}"),
            "different content at a newer bundled path still upgrades",
        )
        assertEquals(digest, ArbdProcessManager.sha256Hex(bundled), "sha256Hex matches MessageDigest")
    }
}

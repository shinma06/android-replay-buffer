package io.github.shinma06.arb.ipc

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.FileTime
import java.time.Instant
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ArbdProcessManagerTest {
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
        if (HostOs.detect() != HostOs.WINDOWS) {
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
}

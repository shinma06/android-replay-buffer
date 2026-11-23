package io.github.shinma06.arb.ipc

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.assertEquals

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
}

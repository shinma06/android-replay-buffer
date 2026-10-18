package io.github.shinma06.replaybuffer.cli

import com.intellij.util.lang.UrlClassLoader
import java.nio.file.Files
import java.nio.file.FileSystems
import java.nio.file.attribute.PosixFilePermissions
import java.nio.file.Path
import java.security.MessageDigest
import java.util.HexFormat
import java.util.concurrent.TimeUnit
import java.util.jar.JarFile
import java.util.zip.ZipFile
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class ReplayCliDistributionTest {
    @TempDir lateinit var temporary: Path

    @Test fun `packaged launchers use Studio runtime and packaged client completes authenticated roundtrip`() {
        val archive = Path.of(System.getProperty("replay.cli.archive"))
        val platform = Path.of(System.getProperty("replay.cli.platform"))
        val extracted = Files.createDirectory(temporary.resolve("distribution with spaces"))
        ZipFile(archive.toFile()).use { zip ->
            for (entry in zip.entries()) {
                val path = extracted.resolve(entry.name).normalize()
                require(path.startsWith(extracted))
                if (entry.isDirectory) Files.createDirectories(path) else {
                    Files.createDirectories(path.parent)
                    zip.getInputStream(entry).use { Files.copy(it, path) }
                }
            }
        }
        val plugin = Files.list(extracted).use { it.toList().single() }
        val replay = plugin.resolve("bin/replay")
        val replayd = plugin.resolve("bin/replayd")
        assertTrue(Files.isRegularFile(replay) && Files.isRegularFile(replayd))
        assertTrue(Files.readString(plugin.resolve("bin/README.txt")).contains(System.getProperty("replay.expected.source")))
        FileSystems.newFileSystem(archive, mapOf("accessMode" to "readOnly")).use { zip ->
            val permissions = PosixFilePermissions.fromString("rwxr-xr-x")
            for (launcher in listOf(replay, replayd)) {
                val entry = zip.getPath("/${extracted.relativize(launcher)}")
                assertEquals(permissions, Files.getAttribute(entry, "zip:permissions"))
                // ZipFile byte extraction does not apply the archive's stored permission bits.
                Files.setPosixFilePermissions(launcher, permissions)
            }
        }
        val java = listOf(platform.resolve("jbr/Contents/Home/bin/java"), platform.resolve("jbr/bin/java"))
            .single { Files.isExecutable(it) }
        val kotlinRuntime = platform.resolve("lib/util-8.jar")
        assertTrue(Files.isRegularFile(kotlinRuntime), "The fixed Studio Kotlin runtime must be present")
        // The Linux CI SDK has a different JBR layout; only the launcher's macOS path layout is a fixture.
        val studio = temporary.resolve("Studio path with spaces/Contents")
        Files.createDirectories(studio.resolve("jbr/Contents/Home/bin"))
        Files.createDirectories(studio.resolve("lib"))
        Files.createSymbolicLink(studio.resolve("jbr/Contents/Home/bin/java"), java)
        Files.createSymbolicLink(studio.resolve("lib/util-8.jar"), kotlinRuntime)
        for (launcher in listOf(replay, replayd)) {
            val result = run(listOf(launcher.toString(), "--help"), mapOf("REPLAY_STUDIO_HOME" to studio.toString()))
            assertEquals(0, result.first, result.second)
            assertTrue(result.second.contains("Usage: replay"))
        }
        assertEquals(2, run(listOf(replay.toString(), "start"), mapOf("REPLAY_STUDIO_HOME" to studio.toString())).first)
        val jar = Files.list(plugin.resolve("lib")).use { files -> files.filter { it.toString().endsWith(".jar") }.toList().single { path ->
            JarFile(path.toFile()).use { it.getJarEntry("io/github/shinma06/replaybuffer/cli/ReplayCli.class") != null }
        } }
        val expectedBuild = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(jar)))
        val loader = UrlClassLoader.build().files(listOf(jar, kotlinRuntime)).parent(ClassLoader.getPlatformClassLoader()).get()
        val registry = loader.loadClass(ReplayCliRegistry::class.java.name)
        assertNull(registry.protectionDomain.codeSource.location, "Exercise the IDE loader without a CodeSource location")
        val build = registry.getMethod("buildIdentity").invoke(registry.getField("INSTANCE").get(null)) as String
        assertEquals(expectedBuild, build)
        val home = Files.createDirectory(temporary.resolve("home"))
        val project = Files.createDirectory(temporary.resolve("project"))
        val server = ReplayCliServer(home.resolve(".android-replay-buffer-cli"), project, build) { request ->
            assertEquals(CliCommand.STATUS, request.command)
            CliResponse(0, "shared result")
        }
        try {
            val result = run(listOf(java.toString(), "-Duser.home=$home", "-cp", "$plugin/lib/*:$kotlinRuntime",
                "io.github.shinma06.replaybuffer.cli.ReplayCli", "--project", project.toString(), "status"))
            assertEquals(0, result.first, result.second)
            assertEquals("shared result\n", result.second)
        } finally {
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
    }

    private fun run(command: List<String>, environment: Map<String, String> = emptyMap()): Pair<Int, String> {
        val output = Files.createTempFile(temporary, "cli-output-", ".txt")
        val builder = ProcessBuilder(command).redirectErrorStream(true).redirectOutput(output.toFile())
        builder.environment().apply {
            remove("JAVA_TOOL_OPTIONS"); remove("JDK_JAVA_OPTIONS"); remove("_JAVA_OPTIONS")
            putAll(environment)
        }
        val process = builder.start()
        try {
            assertTrue(process.waitFor(10, TimeUnit.SECONDS), "Packaged CLI must return without starting an IDE")
            return process.exitValue() to Files.readString(output)
        } finally {
            if (process.isAlive) { process.destroyForcibly(); process.waitFor(5, TimeUnit.SECONDS) }
        }
    }
}

package io.github.shinma06.replaybuffer.cli

import com.intellij.util.lang.UrlClassLoader
import io.github.shinma06.replaybuffer.core.HostPlatform
import java.io.File
import java.math.BigDecimal
import java.nio.file.Files
import java.nio.file.FileSystems
import java.nio.file.attribute.PosixFilePermissions
import java.nio.file.Path
import java.security.MessageDigest
import java.util.HexFormat
import java.util.concurrent.TimeUnit
import java.util.jar.JarFile
import java.util.zip.ZipFile
import kotlin.test.assertContentEquals
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
        val windows = HostPlatform.windows
        val suffix = if (windows) ".cmd" else ""
        val replay = plugin.resolve("bin/replay$suffix")
        val replayd = plugin.resolve("bin/replayd$suffix")
        assertTrue(Files.isRegularFile(replay) && Files.isRegularFile(replayd))
        assertTrue(Files.readString(plugin.resolve("bin/README.txt")).contains(System.getProperty("replay.expected.source")))
        if (!windows) FileSystems.newFileSystem(archive, mapOf("accessMode" to "readOnly")).use { zip ->
            val permissions = PosixFilePermissions.fromString("rwxr-xr-x")
            for (launcher in listOf(replay, replayd)) {
                val entry = zip.getPath("/${extracted.relativize(launcher)}")
                assertEquals(permissions, Files.getAttribute(entry, "zip:permissions"))
                // ZipFile byte extraction does not apply the archive's stored permission bits.
                Files.setPosixFilePermissions(launcher, permissions)
            }
        }
        val java = listOf(platform.resolve("jbr/Contents/Home/bin/java"), platform.resolve("jbr/bin/java"), platform.resolve("jbr/bin/java.exe"))
            .single { Files.isRegularFile(it) && Files.isExecutable(it) }
        val kotlinRuntime = platform.resolve("lib/util-8.jar")
        assertTrue(Files.isRegularFile(kotlinRuntime), "The fixed Studio Kotlin runtime must be present")
        // Link the SDK's own JBR so each launcher resolves its platform layout under a path with spaces.
        val studio = temporary.resolve("Studio path with spaces/Contents")
        Files.createDirectories(studio.resolve("lib"))
        Files.createSymbolicLink(studio.resolve("jbr"), platform.resolve("jbr"))
        Files.createSymbolicLink(studio.resolve("lib/util-8.jar"), kotlinRuntime)
        // Studio keeps JNA's native part outside its jars; the Windows launcher points JNA at it.
        if (Files.isDirectory(platform.resolve("lib/jna"))) Files.createSymbolicLink(studio.resolve("lib/jna"), platform.resolve("lib/jna"))
        for (locale in listOf("C", "en_US.UTF-8")) {
            val environment = mapOf("REPLAY_STUDIO_HOME" to studio.toString(), "LC_ALL" to locale)
            for (launcher in listOf(replay, replayd)) {
                val result = run(listOf(launcher.toString(), "--help"), environment)
                assertEquals(0, result.first, result.third.decodeToString())
                val help = result.second.decodeToString(throwOnInvalidSequence = true)
                assertTrue(help.contains("Usage: replay"))
                assertTrue(help.contains("起動済みAndroid Studioの同じ取得・保存処理を操作します。"))
                assertContentEquals(byteArrayOf(), result.third)
            }
            val invalid = run(listOf(replay.toString(), "start"), environment)
            assertEquals(2, invalid.first)
            assertContentEquals(byteArrayOf(), invalid.second)
            assertContentEquals("不明な操作・optionです。replay --helpを確認してください。\n".toByteArray(Charsets.UTF_8), invalid.third)
        }
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
        val sharedOutput = temporary.resolve("日本語の保存先 with spaces").toString()
        val sharedError = "一部のログがありません。"
        val server = ReplayCliServer(home.resolve(".android-replay-buffer-cli"), project, build) { request ->
            if (request.command == CliCommand.SETTINGS) return@ReplayCliServer CliResponse(0, """{"replay_seconds":180}""")
            assertEquals(CliCommand.STATUS, request.command)
            CliResponse(0, sharedOutput, sharedError)
        }
        try {
            for (locale in listOf("C", "en_US.UTF-8")) {
                // Same JVM options as the launchers: the client's file checks may use JNA.
                val result = run(listOf(java.toString(), "-Duser.home=$home", "--enable-native-access=ALL-UNNAMED", "-cp", "$plugin${File.separator}lib${File.separator}*${File.pathSeparator}$kotlinRuntime",
                    "io.github.shinma06.replaybuffer.cli.ReplayCli", "--project", project.toString(), "status"),
                    mapOf("LC_ALL" to locale))
                assertEquals(0, result.first, result.third.decodeToString())
                assertContentEquals("$sharedOutput\n".toByteArray(Charsets.UTF_8), result.second)
                assertContentEquals("$sharedError\n".toByteArray(Charsets.UTF_8), result.third)
            }
            val session = listOf(
                """{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}""",
                """{"jsonrpc":"2.0","method":"notifications/initialized"}""",
                """{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_settings","arguments":{}}}""",
            ).joinToString("\n", postfix = "\n")
            val mcp = run(listOf(java.toString(), "-Duser.home=$home", "--enable-native-access=ALL-UNNAMED", "-cp", "$plugin${File.separator}lib${File.separator}*${File.pathSeparator}$kotlinRuntime",
                "io.github.shinma06.replaybuffer.cli.ReplayCli", "--project", project.toString(), "mcp"), mapOf("LC_ALL" to "C"), session)
            assertEquals(0, mcp.first, mcp.third.decodeToString())
            val replies = mcp.second.decodeToString(throwOnInvalidSequence = true).lines().filter(String::isNotEmpty)
                .map { McpJson.parse(it) as Map<*, *> }
            assertEquals(2, replies.size, replies.toString())
            val settings = (replies.single { it["id"] == BigDecimal("2") }["result"] as Map<*, *>)
            assertEquals(BigDecimal("180"), (settings["structuredContent"] as Map<*, *>)["replay_seconds"])
        } finally {
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
    }

    private fun run(command: List<String>, environment: Map<String, String> = emptyMap(), stdin: String = ""): Triple<Int, ByteArray, ByteArray> {
        val output = Files.createTempFile(temporary, "cli-output-", ".txt")
        val error = Files.createTempFile(temporary, "cli-error-", ".txt")
        val input = Files.writeString(Files.createTempFile(temporary, "cli-input-", ".txt"), stdin)
        val builder = ProcessBuilder(command).redirectInput(input.toFile()).redirectOutput(output.toFile()).redirectError(error.toFile())
        builder.environment().apply {
            remove("JAVA_TOOL_OPTIONS"); remove("JDK_JAVA_OPTIONS"); remove("_JAVA_OPTIONS")
            putAll(environment)
        }
        val process = builder.start()
        try {
            assertTrue(process.waitFor(10, TimeUnit.SECONDS), "Packaged CLI must return without starting an IDE")
            return Triple(process.exitValue(), Files.readAllBytes(output), Files.readAllBytes(error))
        } finally {
            if (process.isAlive) { process.destroyForcibly(); process.waitFor(5, TimeUnit.SECONDS) }
        }
    }
}

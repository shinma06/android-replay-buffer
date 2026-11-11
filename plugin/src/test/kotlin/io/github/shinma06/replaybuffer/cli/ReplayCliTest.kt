package io.github.shinma06.replaybuffer.cli

import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.settings.patchSettings
import io.github.shinma06.replaybuffer.testing.TestPermissions
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.PrintStream
import java.net.Socket
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.Properties
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class ReplayCliTest {
    @TempDir lateinit var temporary: Path

    @Test fun `commands and quoted path arguments retain a single target`() {
        for (command in listOf("start", "stop", "save", "status", "screenshot", "settings")) {
            assertEquals(command.uppercase(), parseCliArguments(listOf("--project", "a project", command)).command?.name)
        }
        assertEquals(CliArguments(null, Path.of("a project")), parseCliArguments(listOf("--project=a project", "mcp")))
        assertEquals(CliArguments(CliCommand.CONFIGURE, Path.of("p"), """{"replay_seconds":60,"auto_save_on_crash":true,"output_directory":"/a b=c"}"""),
            parseCliArguments(listOf("settings", "set", "replay_seconds=60", "--project", "p", "auto_save_on_crash=true", "output_directory=/a b=c")))
        for (args in listOf(emptyList(), listOf("daemon"), listOf("save", "stop"), listOf("--project"), listOf("--config", "x", "save"),
            listOf("start", "--foreground"), listOf("--daemon"), listOf("--project", "x", "--project", "y", "save"), listOf("--project", "", "stop"),
            listOf("mcp", "status"), listOf("status", "mcp"), listOf("status", "set"), listOf("settings", "get"), listOf("settings", "set"),
            listOf("settings", "set", "replay_seconds"), listOf("settings", "set", "=60"), listOf("settings", "set", "a=1", "a=2"),
            listOf("settings", "set", "output_directory=" + "a".repeat(ReplayCliProtocol.MAX_CONFIG_BYTES)))) {
            assertFailsWith<IllegalArgumentException>(args.toString()) { parseCliArguments(args) }
        }
    }

    @Test fun `settings set sends the MCP update_settings JSON and shares its validation`() {
        val current = ReplaySettings()
        val words = listOf("replay_seconds=60", "auto_save_on_crash=true", "capture_mode=LOG_ONLY", "output_directory=/tmp/out", "manual_package=com.example.app")
        val cli = checkNotNull(parseCliArguments(listOf("settings", "set") + words).settings)
        val mcp = McpJson.write(linkedMapOf("replay_seconds" to java.math.BigDecimal(60), "auto_save_on_crash" to true,
            "capture_mode" to "LOG_ONLY", "output_directory" to "/tmp/out", "manual_package" to "com.example.app"))
        assertEquals(mcp, cli)
        assertEquals(patchSettings(current, mcp), patchSettings(current, cli))
        assertEquals(60, patchSettings(current, cli).retentionSeconds)
        for ((word, message) in listOf("replay_seconds=abc" to "replay_secondsは整数で指定してください。",
            "replay_seconds=99999999999" to "replay_secondsは整数で指定してください。",
            "show_touches=yes" to "show_touchesはtrue/falseで指定してください。",
            "record_audio=yes" to "record_audioはtrue/falseで指定してください。")) {
            val text = checkNotNull(parseCliArguments(listOf("settings", "set", word)).settings)
            assertEquals(message, assertFailsWith<IllegalArgumentException> { patchSettings(current, text) }.message)
        }
        assertFailsWith<IllegalArgumentException> { patchSettings(current, parseCliArguments(listOf("settings", "set", "project=/x")).settings!!) }
    }

    @Test fun `running IDE records its Studio home owner-only without becoming an endpoint`() {
        val registry = temporary.resolve("registry")
        val first = temporary.resolve("Studio one/Contents")
        ReplayCliRegistry.recordStudioHome(registry, first)
        val record = registry.resolve(ReplayCliRegistry.STUDIO_HOME_FILE)
        assertEquals(first.toAbsolutePath().normalize().toString(), Files.readString(record))
        assertEquals(TestPermissions.mode("rwx------"), TestPermissions.get(registry))
        assertEquals(TestPermissions.mode("rw-------"), TestPermissions.get(record))
        val second = temporary.resolve("スタジオ 2")
        ReplayCliRegistry.recordStudioHome(registry, second.resolve("..").resolve("スタジオ 2"))
        assertEquals(second.toString(), Files.readString(record))
        assertEquals(TestPermissions.mode("rw-------"), TestPermissions.get(record))
        assertFailsWith<IllegalArgumentException> { ReplayCliRegistry.recordStudioHome(registry, Path.of(temporary.toString(), "line\nbreak")) }
        assertEquals(second.toString(), Files.readString(record))
        assertEquals(listOf(ReplayCliRegistry.STUDIO_HOME_FILE), Files.list(registry).use { files -> files.map { it.fileName.toString() }.toList() })
        val project = Files.createDirectory(temporary.resolve("project"))
        ReplayCliServer(registry, project, "build") { CliResponse(0, "ok") }.use { server ->
            assertEquals(server.endpoint.id, ReplayCliRegistry.select(registry, null, "build").id)
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
        assertEquals(second.toString(), Files.readString(record))
        val shared = Files.createDirectory(temporary.resolve("shared")).also { TestPermissions.set(it, "rwxrwxrwx") }
        assertFailsWith<IllegalArgumentException> { ReplayCliRegistry.recordStudioHome(shared, first) }
        assertFalse(Files.exists(shared.resolve(ReplayCliRegistry.STUDIO_HOME_FILE)))
    }

    @Test fun `protocol rejects negative oversized invalid UTF8 and unsupported response`() {
        for (size in listOf(-1, 65537)) {
            val bytes = ByteArrayOutputStream().also { DataOutputStream(it).writeInt(size) }.toByteArray()
            assertFailsWith<IllegalArgumentException> { ReplayCliProtocol.readText(DataInputStream(bytes.inputStream())) }
        }
        val malformed = ByteArrayOutputStream().also { DataOutputStream(it).apply { writeInt(1); writeByte(0xff) } }.toByteArray()
        assertFailsWith<java.nio.charset.CharacterCodingException> { ReplayCliProtocol.readText(DataInputStream(malformed.inputStream())) }
        val code = ByteArrayOutputStream().also { DataOutputStream(it).writeInt(3) }.toByteArray()
        assertFailsWith<IllegalArgumentException> { ReplayCliProtocol.readResponse(DataInputStream(code.inputStream())) }
    }

    @Test fun `one authenticated client receives exact output and server error`() {
        val project = Files.createDirectory(temporary.resolve("project with spaces"))
        val registry = temporary.resolve("registry")
        val calls = AtomicInteger()
        ReplayCliServer(registry, project, "build") { request ->
            calls.incrementAndGet()
            assertEquals(CliCommand.CONFIGURE, request.command)
            assertEquals("""{"replay_seconds":901}""", request.configText)
            CliResponse(2, error = "保持時間は1〜900秒で指定してください。")
        }.use { server ->
            assertEquals(server.endpoint.id, ReplayCliRegistry.select(registry, project, "build").id)
            assertFailsWith<java.nio.file.FileAlreadyExistsException> { ReplayCliRegistry.publish(registry, server.endpoint.copy(build = "replacement")) }
            assertEquals("build", ReplayCliRegistry.read(registry.resolve("${server.endpoint.id}.properties")).build)
            val (code, output, error) = runCli(registry, listOf("--project", project.toString(), "settings", "set", "replay_seconds=901"))
            assertEquals(2, code)
            assertEquals("", output)
            assertEquals("保持時間は1〜900秒で指定してください。\n", error)
            assertEquals(1, calls.get())
            assertEquals(TestPermissions.mode("rwx------"), TestPermissions.get(registry))
            Files.list(registry).use { files -> files.forEach {
                assertEquals(TestPermissions.mode("rw-------"), TestPermissions.get(it))
            } }
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
        assertEquals(0L, Files.list(registry).use { it.count() })
    }

    @Test fun `unauthenticated input cannot dispatch and substituted server cannot receive request`() {
        val registry = temporary.resolve("registry")
        val calls = AtomicInteger()
        ReplayCliServer(registry, temporary, "build") { calls.incrementAndGet(); CliResponse(0) }.use { server ->
            Socket("127.0.0.1", server.endpoint.port).use { socket ->
                socket.soTimeout = 2000
                assertFailsWith<IllegalArgumentException> {
                    ReplayCliProtocol.authenticateClient(DataInputStream(socket.getInputStream()), DataOutputStream(socket.getOutputStream()), ByteArray(32))
                }
            }
            assertEquals(0, calls.get())
        }
    }

    @Test fun `ambiguous absent stale insecure and mismatched targets never dispatch`() {
        val registry = temporary.resolve("registry")
        val one = Files.createDirectory(temporary.resolve("one"))
        val two = Files.createDirectory(temporary.resolve("two"))
        val calls = AtomicInteger()
        ReplayCliServer(registry, one, "build") { calls.incrementAndGet(); CliResponse(0, "one") }.use { first ->
            ReplayCliServer(registry, two, "build") { calls.incrementAndGet(); CliResponse(0, "two") }.use {
                assertEquals(1, runCli(registry, listOf("status")).first)
                assertEquals(1, runCli(registry, listOf("--project", temporary.toString(), "stop")).first)
                assertEquals(1, runCli(registry, listOf("--project", one.toString(), "status"), "other-build").first)
                assertEquals(0, runCli(registry, listOf("--project", one.toString(), "status")).first)
                assertEquals(1, calls.get())
            }
            val descriptor = registry.resolve("${first.endpoint.id}.properties")
            TestPermissions.set(descriptor, TestPermissions.mode("rw-r--r--"))
            assertEquals(1, runCli(registry, listOf("status")).first)
            TestPermissions.set(descriptor, TestPermissions.mode("rw-------"))
            Files.delete(descriptor)
            ReplayCliRegistry.publish(registry, first.endpoint.copy(started = first.endpoint.started.minusSeconds(1)))
            assertEquals(1, runCli(registry, listOf("status")).first)
            assertEquals(1, calls.get())
        }
    }

    @Test fun `malformed registry data is rejected without echoing untrusted content`() {
        val registry = temporary.resolve("registry")
        val calls = AtomicInteger()
        val server = ReplayCliServer(registry, temporary, "build") { calls.incrementAndGet(); CliResponse(0) }
        val path = registry.resolve("${server.endpoint.id}.properties")
        val original = Files.readAllBytes(path)
        try {
            for ((key, value) in mapOf("id" to "\u001b[31mUNTRUSTED", "port" to "UNTRUSTED", "pid" to "UNTRUSTED",
                "started" to "UNTRUSTED", "project" to "\u0000UNTRUSTED")) {
                val properties = Properties().apply { load(original.inputStream()); setProperty(key, value) }
                Files.newOutputStream(path).use { properties.store(it, null) }
                val (code, output, error) = runCli(registry, listOf("stop"))
                assertEquals(1, code)
                assertEquals("", output)
                assertFalse(error.contains("UNTRUSTED"))
                assertEquals(0, calls.get())
            }
        } finally {
            Files.write(path, original)
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
    }

    @Test fun `authenticated malformed command and forbidden config cannot dispatch`() {
        val registry = temporary.resolve("registry")
        val calls = AtomicInteger()
        val server = ReplayCliServer(registry, temporary, "build") { calls.incrementAndGet(); CliResponse(0) }
        try {
            for ((command, config) in listOf("UNKNOWN" to "{}", "STOP" to "{}", "SETTINGS" to "{}", "CONFIGURE" to null)) {
                Socket("127.0.0.1", server.endpoint.port).use { socket ->
                    socket.soTimeout = 2000
                    val input = DataInputStream(socket.getInputStream())
                    val output = DataOutputStream(socket.getOutputStream())
                    ReplayCliProtocol.authenticateClient(input, output, server.endpoint.token)
                    repeat(3) { ReplayCliProtocol.readText(input) }
                    val disconnected = try {
                        ReplayCliProtocol.writeText(output, command)
                        output.writeBoolean(config != null)
                        config?.let { ReplayCliProtocol.writeText(output, it) }
                        output.flush()
                        input.read() == -1
                    } catch (_: java.net.SocketException) { true }
                    assertTrue(disconnected)
                }
            }
            assertEquals(0, calls.get())
        } finally {
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
    }

    @Test fun `registry symlinks are rejected without command`() {
        val actual = Files.createDirectory(temporary.resolve("actual")).also { TestPermissions.set(it, "rwx------") }
        val link = Files.createSymbolicLink(temporary.resolve("link"), actual)
        assertFailsWith<IllegalArgumentException> { ReplayCliRegistry.prepare(link) }
    }

    @Test fun `disconnect after dispatch gives unknown result and never repeats mutation`() {
        val registry = temporary.resolve("registry")
        val dispatched = CountDownLatch(1)
        val release = CountDownLatch(1)
        val calls = AtomicInteger()
        val server = ReplayCliServer(registry, temporary, "build") {
            calls.incrementAndGet()
            dispatched.countDown()
            release.await(5, TimeUnit.SECONDS)
            CliResponse(0, "completed")
        }
        var result: Triple<Int, String, String>? = null
        val client = thread { result = runCli(registry, listOf("save")) }
        try {
            assertTrue(dispatched.await(5, TimeUnit.SECONDS))
            server.close()
            client.join(5000)
            assertFalse(client.isAlive)
            assertEquals(1, result?.first)
            assertTrue(result?.third.orEmpty().contains("操作結果を確認できません"))
            assertEquals(1, calls.get())
        } finally {
            release.countDown()
            server.close()
            client.join(5000)
        }
    }

    @Test fun `trickling incomplete request expires and releases its worker without dispatch`() {
        val registry = temporary.resolve("registry")
        val calls = AtomicInteger()
        val server = ReplayCliServer(registry, temporary, "build") { calls.incrementAndGet(); CliResponse(0) }
        try {
            Socket("127.0.0.1", server.endpoint.port).use { socket ->
                socket.soTimeout = 8000
                val input = DataInputStream(socket.getInputStream())
                val output = DataOutputStream(socket.getOutputStream())
                ReplayCliProtocol.authenticateClient(input, output, server.endpoint.token)
                repeat(3) { ReplayCliProtocol.readText(input) }
                output.writeInt(16)
                output.flush()
                // Each byte is within the idle timeout; the complete request still has one deadline.
                val trickle = thread {
                    try {
                        repeat(7) { output.writeByte(0); output.flush(); Thread.sleep(1000) }
                    } catch (_: java.io.IOException) { }
                    catch (_: InterruptedException) { Thread.currentThread().interrupt() }
                }
                try {
                    val closed = try { input.read() == -1 } catch (_: java.net.SocketException) { true }
                    assertTrue(closed)
                    assertEquals(0, calls.get())
                } finally { trickle.interrupt(); trickle.join(2000); assertFalse(trickle.isAlive) }
            }
            assertEquals(0, runCli(registry, listOf("status")).first)
            assertEquals(1, calls.get())
        } finally {
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
    }

    @Test fun `four request workers bound concurrent clients without queued mutations`() {
        val registry = temporary.resolve("registry")
        val entered = CountDownLatch(4)
        val release = CountDownLatch(1)
        val calls = AtomicInteger()
        val server = ReplayCliServer(registry, temporary, "build") {
            calls.incrementAndGet()
            entered.countDown()
            release.await(5, TimeUnit.SECONDS)
            CliResponse(0)
        }
        val clients = List(4) { thread { runCli(registry, listOf("status")) } }
        try {
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            assertEquals(1, runCli(registry, listOf("stop")).first)
            assertEquals(4, calls.get())
        } finally {
            release.countDown()
            clients.forEach { it.join(5000); assertFalse(it.isAlive) }
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
    }

    @Test fun `invalid settings arguments fail before any connection`() {
        val none = temporary.resolve("none")
        assertEquals(2, runCli(none, listOf("settings", "set")).first)
        assertEquals(2, runCli(none, listOf("settings", "set", "replay_seconds")).first)
        assertEquals(2, runCli(none, listOf("settings", "set", "output_directory=" + "a".repeat(ReplayCliProtocol.MAX_CONFIG_BYTES))).first)
        assertEquals(1, runCli(none, listOf("settings")).first)
        assertEquals(1, runCli(none, listOf("settings", "set", "replay_seconds=60")).first)
    }

    @Test fun `help is offline and argument errors remain separate from connection errors`() {
        assertEquals(0, runCli(temporary.resolve("missing"), listOf("--help")).first)
        assertEquals(2, runCli(temporary.resolve("missing"), listOf("daemon")).first)
        assertEquals(1, runCli(temporary.resolve("missing"), listOf("status")).first)
        val error = ByteArrayOutputStream()
        val code = ReplayCli.run(listOf("status"), PrintStream(ByteArrayOutputStream()), PrintStream(error, true, Charsets.UTF_8),
            temporary.resolve("missing")) { error("配布物のCLIを利用してください。") }
        assertEquals(1, code)
        assertEquals("配布物のCLIを利用してください。\n", error.toString(Charsets.UTF_8))
    }

    private fun runCli(registry: Path, args: List<String>, build: String = "build"): Triple<Int, String, String> {
        val output = ByteArrayOutputStream()
        val error = ByteArrayOutputStream()
        val code = ReplayCli.run(args, PrintStream(output, true, Charsets.UTF_8), PrintStream(error, true, Charsets.UTF_8), registry) { build }
        return Triple(code, output.toString(Charsets.UTF_8), error.toString(Charsets.UTF_8))
    }
}

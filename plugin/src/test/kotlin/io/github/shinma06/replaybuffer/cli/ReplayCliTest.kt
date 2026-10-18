package io.github.shinma06.replaybuffer.cli

import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.PrintStream
import java.net.Socket
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.PosixFilePermissions
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

    @Test fun `legacy commands and quoted path arguments retain a single target`() {
        for (command in listOf("save", "status", "stop", "daemon")) {
            assertEquals(command.uppercase(), parseCliArguments(listOf("--project", "a project", command)).command.name)
        }
        assertEquals(Path.of("old config.json"), parseCliArguments(listOf("--config=old config.json", "daemon")).config)
        assertEquals(Path.of("old config.json"), parseCliArguments(listOf("daemon", "--foreground", "--config", "old config.json")).config)
        for (args in listOf(emptyList(), listOf("start"), listOf("save", "stop"), listOf("--project"),
            listOf("status", "--config", "x"), listOf("save", "--foreground"), listOf("daemon", "--daemon"),
            listOf("--project", "x", "--project", "y", "save"), listOf("--project", "", "stop"))) {
            assertFailsWith<Exception> { parseCliArguments(args) }
        }
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
        val config = temporary.resolve("old config.yaml")
        Files.writeString(config, "replay_seconds: 60")
        val registry = temporary.resolve("registry")
        val calls = AtomicInteger()
        ReplayCliServer(registry, project, "build") { request ->
            calls.incrementAndGet()
            assertEquals(CliCommand.SAVE, request.command)
            assertTrue(request.yaml)
            assertEquals("replay_seconds: 60", request.configText)
            assertTrue(Path.of(request.workingDirectory).isAbsolute)
            CliResponse(1, error = "保存先へ書き込めません。")
        }.use { server ->
            assertEquals(server.endpoint.id, ReplayCliRegistry.select(registry, project, "build").id)
            assertFailsWith<java.nio.file.FileAlreadyExistsException> { ReplayCliRegistry.publish(registry, server.endpoint.copy(build = "replacement")) }
            assertEquals("build", ReplayCliRegistry.read(registry.resolve("${server.endpoint.id}.properties")).build)
            val (code, output, error) = runCli(registry, listOf("--project", project.toString(), "--config", config.toString(), "save"))
            assertEquals(1, code)
            assertEquals("", output)
            assertTrue(error.contains("保存先へ書き込めません。"))
            assertEquals(1, calls.get())
            assertEquals(PosixFilePermissions.fromString("rwx------"), Files.getPosixFilePermissions(registry))
            Files.list(registry).use { files -> files.forEach {
                assertEquals(PosixFilePermissions.fromString("rw-------"), Files.getPosixFilePermissions(it))
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
            Files.setPosixFilePermissions(descriptor, PosixFilePermissions.fromString("rw-r--r--"))
            assertEquals(1, runCli(registry, listOf("status")).first)
            Files.setPosixFilePermissions(descriptor, PosixFilePermissions.fromString("rw-------"))
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
            for (command in listOf("UNKNOWN", "STOP")) {
                Socket("127.0.0.1", server.endpoint.port).use { socket ->
                    socket.soTimeout = 2000
                    val input = DataInputStream(socket.getInputStream())
                    val output = DataOutputStream(socket.getOutputStream())
                    ReplayCliProtocol.authenticateClient(input, output, server.endpoint.token)
                    repeat(3) { ReplayCliProtocol.readText(input) }
                    val disconnected = try {
                        ReplayCliProtocol.writeText(output, command)
                        output.writeBoolean(true)
                        ReplayCliProtocol.writeText(output, "{}")
                        output.writeBoolean(false)
                        ReplayCliProtocol.writeText(output, temporary.toString())
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

    @Test fun `registry symlinks and nonregular config are rejected without command`() {
        val actual = Files.createDirectory(temporary.resolve("actual"), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
        val link = Files.createSymbolicLink(temporary.resolve("link"), actual)
        assertFailsWith<IllegalArgumentException> { ReplayCliRegistry.prepare(link) }
        assertEquals(2, runCli(temporary.resolve("none"), listOf("daemon", "--config", temporary.toString())).first)
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

    @Test fun `invalid configuration bytes and sizes fail before any connection`() {
        val config = temporary.resolve("config.json")
        Files.write(config, byteArrayOf(0xff.toByte()))
        assertEquals(2, runCli(temporary.resolve("none"), listOf("daemon", "--config", config.toString())).first)
        Files.writeString(config, " ".repeat(65537))
        assertEquals(2, runCli(temporary.resolve("none"), listOf("daemon", "--config", config.toString())).first)
        Files.delete(config)
        assertEquals(1, runCli(temporary.resolve("none"), listOf("daemon", "--config", config.toString())).first)
    }

    @Test fun `help is offline and argument errors remain separate from connection errors`() {
        assertEquals(0, runCli(temporary.resolve("missing"), listOf("--help")).first)
        assertEquals(2, runCli(temporary.resolve("missing"), listOf("start")).first)
        assertEquals(1, runCli(temporary.resolve("missing"), listOf("status")).first)
    }

    private fun runCli(registry: Path, args: List<String>, build: String = "build"): Triple<Int, String, String> {
        val output = ByteArrayOutputStream()
        val error = ByteArrayOutputStream()
        val code = ReplayCli.run(args, PrintStream(output, true, Charsets.UTF_8), PrintStream(error, true, Charsets.UTF_8), registry) { build }
        return Triple(code, output.toString(Charsets.UTF_8), error.toString(Charsets.UTF_8))
    }
}

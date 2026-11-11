package io.github.shinma06.replaybuffer.cli

import java.io.ByteArrayOutputStream
import java.io.PrintStream
import java.math.BigDecimal
import java.nio.file.Files
import java.nio.file.Path
import java.util.Base64
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class ReplayMcpServerTest {
    @TempDir lateinit var temporary: Path

    private fun session(lines: List<String>, defaultProject: Path? = null, registry: Path = temporary.resolve("registry"),
        send: (Path, Path?, String, CliRequest) -> CliResponse): Map<Any?, Map<*, *>> {
        val output = ByteArrayOutputStream()
        val input = lines.joinToString("\n", postfix = "\n").toByteArray(Charsets.UTF_8).inputStream()
        val code = ReplayMcpServer(input, output, PrintStream(ByteArrayOutputStream()), defaultProject, registry, { "build" }, send).run()
        assertEquals(0, code)
        return output.toString(Charsets.UTF_8).lines().filter(String::isNotEmpty).map { McpJson.parse(it) as Map<*, *> }
            .associateBy { it["id"] }
    }

    private fun call(id: Int, tool: String, arguments: String = "{}") =
        """{"jsonrpc":"2.0","id":$id,"method":"tools/call","params":{"name":"$tool","arguments":$arguments}}"""

    private val initialize = """{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"""

    private fun Map<*, *>.result() = this["result"] as Map<*, *>
    private fun Map<*, *>.text() = ((result()["content"] as List<*>).first() as Map<*, *>)["text"] as String

    @Test fun `save_replay logs_only maps onto the logs-only CLI save`() {
        val requests = mutableListOf<CliRequest>()
        val responses = session(listOf(
            initialize,
            """{"jsonrpc":"2.0","id":"list","method":"tools/list"}""",
            call(1, "save_replay", """{"logs_only":true}"""),
            call(2, "save_replay"),
            call(3, "save_replay", """{"logs_only":"yes"}"""),
        ), temporary.resolve("project")) { _, _, _, request -> requests += request; CliResponse(0, "/saved") }
        assertEquals(listOf(CliCommand.SAVE_LOGS, CliCommand.SAVE), requests.map { it.command })
        assertEquals("/saved", (responses[BigDecimal("1")]!!.result()["structuredContent"] as Map<*, *>)["directory"])
        assertEquals(true, responses[BigDecimal("3")]!!.result()["isError"])
        val save = ((responses["list"]!!.result()["tools"]) as List<*>).map { it as Map<*, *> }.single { it["name"] == "save_replay" }
        val properties = (save["inputSchema"] as Map<*, *>)["properties"] as Map<*, *>
        assertEquals("boolean", (properties["logs_only"] as Map<*, *>)["type"])
    }

    @Test fun `scripted session maps tools onto CLI requests and results`() {
        val requests = mutableListOf<CliRequest>()
        val project = temporary.resolve("project")
        val png = temporary.resolve("shot.png").also { Files.write(it, byteArrayOf(1, 2, 3)) }
        val responses = session(listOf(
            initialize,
            """{"jsonrpc":"2.0","method":"notifications/initialized"}""",
            """{"jsonrpc":"2.0","id":"list","method":"tools/list"}""",
            """{"jsonrpc":"2.0","id":"p","method":"ping"}""",
            call(1, "get_status"),
            call(2, "save_replay", """{"project":"/other"}"""),
            call(3, "take_screenshot"),
            call(4, "update_settings", """{"replay_seconds":30,"auto_save_on_crash":true}"""),
            call(5, "start_capture", """{"unknown":1}"""),
            call(6, "stop_capture"),
            call(9, "stop_capture", """{"project":null}"""),
            """{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"missing"}}""",
            """{"jsonrpc":"2.0","id":8,"method":"resources/list"}""",
            "{not json",
        ), project) { _, target, build, request ->
            assertEquals("build", build)
            synchronized(requests) { requests += request }
            when (request.command) {
                CliCommand.STATUS -> { assertEquals(project, target); CliResponse(0, """{"running":true,"capture_state":"CAPTURING"}""") }
                CliCommand.SAVE -> { assertEquals(Path.of("/other"), target); CliResponse(1, error = "保存できるデータがありません。") }
                CliCommand.SCREENSHOT -> CliResponse(0, png.toString())
                CliCommand.CONFIGURE -> CliResponse(0, """{"replay_seconds":30}""")
                CliCommand.STOP -> CliResponse(0, "stopped")
                else -> error("unexpected ${request.command}")
            }
        }
        val init = responses[BigDecimal("0")]!!.result()
        assertEquals("2025-06-18", init["protocolVersion"])
        assertEquals("android-replay-buffer", (init["serverInfo"] as Map<*, *>)["name"])
        val names = ((responses["list"]!!.result()["tools"]) as List<*>).map { (it as Map<*, *>)["name"] }
        assertEquals(listOf("list_projects", "get_status", "start_capture", "stop_capture", "save_replay", "take_screenshot",
            "get_settings", "update_settings"), names)
        assertEquals(emptyMap<Any, Any>(), responses["p"]!!.result())
        val status = responses[BigDecimal("1")]!!.result()
        assertEquals(false, status["isError"])
        assertEquals(true, (status["structuredContent"] as Map<*, *>)["running"])
        val save = responses[BigDecimal("2")]!!
        assertEquals(true, save.result()["isError"])
        assertTrue(save.text().contains("保存できるデータがありません。") && save.text().contains("exit code 1"))
        val shot = responses[BigDecimal("3")]!!.result()
        val image = (shot["content"] as List<*>)[1] as Map<*, *>
        assertEquals("image/png", image["mimeType"])
        assertEquals(listOf<Byte>(1, 2, 3), Base64.getDecoder().decode(image["data"] as String).toList())
        assertEquals(png.toString(), (shot["structuredContent"] as Map<*, *>)["path"])
        assertEquals(false, responses[BigDecimal("4")]!!.result()["isError"])
        assertEquals(mapOf("replay_seconds" to BigDecimal("30"), "auto_save_on_crash" to true),
            McpJson.parse(requests.single { it.command == CliCommand.CONFIGURE }.configText!!))
        assertTrue(responses[BigDecimal("5")]!!.text().contains("exit code 2"))
        assertEquals("stopped", responses[BigDecimal("6")]!!.text())
        assertEquals(BigDecimal("-32602"), (responses[BigDecimal("7")]!!["error"] as Map<*, *>)["code"])
        assertEquals(BigDecimal("-32601"), (responses[BigDecimal("8")]!!["error"] as Map<*, *>)["code"])
        assertTrue(responses[BigDecimal("9")]!!.text().contains("exit code 2"))
        assertEquals(BigDecimal("-32700"), (responses[null]!!["error"] as Map<*, *>)["code"])
        assertFalse(requests.any { it.command == CliCommand.START }, "Invalid arguments must not dispatch")
        assertEquals(5, requests.size)
    }

    @Test fun `older protocol omits structured content and unknown versions negotiate the latest`() {
        val old = session(listOf(initialize.replace("2025-06-18", "2024-11-05"), call(1, "get_status"))) { _, _, _, _ -> CliResponse(0, "{}") }
        assertEquals("2024-11-05", old[BigDecimal("0")]!!.result()["protocolVersion"])
        assertNull(old[BigDecimal("1")]!!.result()["structuredContent"])
        val future = session(listOf(initialize.replace("2025-06-18", "2099-01-01"))) { _, _, _, _ -> CliResponse(0) }
        assertEquals(ReplayMcpServer.LATEST_VERSION, future[BigDecimal("0")]!!.result()["protocolVersion"])
    }

    @Test fun `real endpoint session lists projects and reaches the IDE request handler`() {
        val registry = temporary.resolve("registry")
        val project = Files.createDirectory(temporary.resolve("project with spaces"))
        val server = ReplayCliServer(registry, project, "build") { request ->
            assertEquals(CliCommand.SETTINGS, request.command)
            CliResponse(0, """{"replay_seconds":180,"output_directory":""}""")
        }
        try {
            val responses = session(listOf(initialize, """{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_projects"}}""",
                call(2, "get_settings")), registry = registry) { directory, target, build, request ->
                ReplayCli.exchange(directory, target, build, request)
            }
            val projects = (responses[BigDecimal("1")]!!.result()["structuredContent"] as Map<*, *>)["projects"] as List<*>
            assertEquals(listOf(mapOf("project" to project.toRealPath().toString(), "same_build" to true)), projects)
            assertEquals(BigDecimal("180"), (responses[BigDecimal("2")]!!.result()["structuredContent"] as Map<*, *>)["replay_seconds"])
        } finally {
            server.close()
            server.termination.get(5, TimeUnit.SECONDS)
        }
    }
}

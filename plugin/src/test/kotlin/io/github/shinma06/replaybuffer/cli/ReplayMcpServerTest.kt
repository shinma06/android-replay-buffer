package io.github.shinma06.replaybuffer.cli

import java.io.ByteArrayOutputStream
import java.io.PrintStream
import java.math.BigDecimal
import java.nio.file.Files
import java.nio.file.Path
import java.util.Base64
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class ReplayMcpServerTest {
    @Test fun `trim logs before execution and honors the negotiated logging level`() {
        val levels = listOf("debug", "info", "notice", "warning", "error", "critical", "alert", "emergency")
        for (level in listOf<String?>(null) + levels) {
            val output = ByteArrayOutputStream()
            val requests = mutableListOf<String>()
            var notificationBeforeExecution = false
            val lines = listOfNotNull(initialize,
                level?.let { """{"jsonrpc":"2.0","id":"level","method":"logging/setLevel","params":{"level":"$it"}}""" },
                call(1, "trim_replay", """{"folder":"/saved","from_seconds":0,"to_seconds":1}"""))
            ReplayMcpServer(lines.joinToString("\n", postfix = "\n").byteInputStream(), java.io.BufferedOutputStream(output),
                PrintStream(ByteArrayOutputStream()), null, temporary, { "build" }, { _, _, _, request ->
                    requests += request.command.name
                    if (request.command.name == "TRIM_PREFLIGHT")
                        CliResponse(0, """{"fingerprint":"snapshot","warning":"時計対応が不明です。"}""")
                    else {
                        notificationBeforeExecution = output.toString(Charsets.UTF_8).contains("notifications/message")
                        CliResponse(0, "/saved-trim")
                    }
                }).run()
            val messages = output.toString(Charsets.UTF_8).lineSequence().filter(String::isNotEmpty)
                .map { McpJson.parse(it) as Map<*, *> }.toList()
            val capabilities = messages.first().result()["capabilities"] as Map<*, *>
            assertTrue("logging" in capabilities)
            assertEquals(listOf("TRIM_PREFLIGHT", "TRIM"), requests)
            val enabled = level == null || levels.indexOf(level) <= levels.indexOf("warning")
            assertEquals(enabled, notificationBeforeExecution)
            assertEquals(if (enabled) 1 else 0, messages.count { it["method"] == "notifications/message" })
            if (level != null) assertEquals(emptyMap<String, Any>(), messages.single { it["id"] == "level" }.result())
            messages.singleOrNull { it["method"] == "notifications/message" }?.let { message ->
                val params = message["params"] as Map<*, *>
                assertEquals("warning", params["level"])
                assertEquals("replay.trim", params["logger"])
                assertEquals("時計対応が不明です。", params["data"])
            }
            val result = messages.single { it["id"] == BigDecimal.ONE }.result()
            assertEquals(false, result["isError"])
            assertEquals(mapOf("directory" to "/saved-trim", "summary_path" to Path.of("/saved-trim").resolve("README.md").toString()), result["structuredContent"])
        }
    }

    @TempDir lateinit var temporary: Path

    @Test fun `logging rejects invalid levels without dispatching an operation`() {
        val responses = session(listOf(initialize) + listOf("\"verbose\"", "null", "1").mapIndexed { index, level ->
            """{"jsonrpc":"2.0","id":${index + 1},"method":"logging/setLevel","params":{"level":$level}}"""
        }) { _, _, _, _ -> error("logging must not execute an IDE operation") }
        for (id in 1..3) assertEquals(BigDecimal("-32602"), (responses[BigDecimal(id)]!!["error"] as Map<*, *>)["code"])
    }

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
        ), temporary.resolve("project")) { _, _, _, request ->
            synchronized(requests) { requests += request }
            CliResponse(0, if (request.command == CliCommand.SAVE_LOGS) "/saved-logs" else "/saved")
        }
        // Tool calls run concurrently (docs/mcp.md), so each request is matched to its JSON-RPC id, not to arrival order.
        assertEquals(listOf(CliCommand.SAVE, CliCommand.SAVE_LOGS), requests.map { it.command }.sorted())
        val logsOnly = responses[BigDecimal("1")]!!.result()["structuredContent"] as Map<*, *>
        assertEquals("/saved-logs", logsOnly["directory"])
        assertEquals(Path.of("/saved-logs").resolve("README.md").toString(), logsOnly["summary_path"])
        assertEquals("/saved", (responses[BigDecimal("2")]!!.result()["structuredContent"] as Map<*, *>)["directory"])
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
            call(10, "trim_replay", """{"folder":"/saved","from_seconds":0.5,"to_seconds":2}"""),
            call(11, "export_frame", """{"folder":"/saved","at_seconds":1.25}"""),
            call(12, "export_frame", """{"folder":"/saved","at_seconds":1,"include_image":false}"""),
            call(13, "trim_replay", """{"folder":"/saved","from_seconds":2,"to_seconds":1}"""),
            call(14, "export_frame", """{"folder":"/saved","at_seconds":"x"}"""),
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
                CliCommand.TRIM -> CliResponse(0, "/saved-trim")
                CliCommand.TRIM_PREFLIGHT -> CliResponse(0, """{"fingerprint":"snapshot","warning":null}""")
                CliCommand.FRAME -> CliResponse(0, png.toString())
                else -> error("unexpected ${request.command}")
            }
        }
        val init = responses[BigDecimal("0")]!!.result()
        assertEquals("2025-06-18", init["protocolVersion"])
        assertEquals("android-replay-buffer", (init["serverInfo"] as Map<*, *>)["name"])
        val names = ((responses["list"]!!.result()["tools"]) as List<*>).map { (it as Map<*, *>)["name"] }
        assertEquals(listOf("list_projects", "get_status", "start_capture", "stop_capture", "save_replay", "take_screenshot",
            "trim_replay", "export_frame", "get_settings", "update_settings"), names)
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
        val trim = responses[BigDecimal("10")]!!.result()
        assertEquals(false, trim["isError"])
        assertEquals("/saved-trim", (trim["structuredContent"] as Map<*, *>)["directory"])
        assertEquals(Path.of("/saved-trim").resolve("README.md").toString(), (trim["structuredContent"] as Map<*, *>)["summary_path"])
        assertEquals("""{"folder":"/saved","from_us":500000,"to_us":2000000,"fingerprint":"snapshot"}""",
            requests.single { it.command == CliCommand.TRIM }.configText)
        val frame = responses[BigDecimal("11")]!!.result()
        assertEquals("image/png", ((frame["content"] as List<*>)[1] as Map<*, *>)["mimeType"])
        assertEquals(png.toString(), (frame["structuredContent"] as Map<*, *>)["path"])
        assertEquals(listOf("""{"folder":"/saved","at_us":1000000}""", """{"folder":"/saved","at_us":1250000}"""),
            requests.filter { it.command == CliCommand.FRAME }.mapNotNull { it.configText }.sorted())
        val frameNoImage = responses[BigDecimal("12")]!!.result()
        assertEquals(1, (frameNoImage["content"] as List<*>).size)
        assertEquals(png.toString(), (frameNoImage["structuredContent"] as Map<*, *>)["path"])
        assertEquals(true, responses[BigDecimal("13")]!!.result()["isError"])
        assertEquals(true, responses[BigDecimal("14")]!!.result()["isError"])
        assertFalse(requests.any { it.command == CliCommand.START }, "Invalid arguments must not dispatch")
        assertEquals(9, requests.size)
    }

    private fun queuedSession(count: Int, callLimitMs: Long, send: (Path, Path?, String, CliRequest) -> CliResponse): List<Map<*, *>> {
        val output = ByteArrayOutputStream()
        val input = (1..count).joinToString("\n", postfix = "\n") { call(it, "save_replay") }.toByteArray(Charsets.UTF_8).inputStream()
        val code = ReplayMcpServer(input, output, PrintStream(ByteArrayOutputStream()), temporary, temporary.resolve("registry"),
            { "build" }, send, callLimitMs).run()
        assertEquals(0, code)
        return output.toString(Charsets.UTF_8).lines().filter(String::isNotEmpty).map { McpJson.parse(it) as Map<*, *> }
    }

    @Test fun `stdin EOF waits for every queued call and answers each one exactly once`() {
        val sent = AtomicInteger()
        // 4 running + 16 queued calls take 5 rounds, longer than one call's limit but within each call's own limit.
        val responses = queuedSession(20, callLimitMs = 1_500) { _, _, _, _ ->
            Thread.sleep(500)
            CliResponse(0, output = "/saved/${sent.incrementAndGet()}")
        }
        assertEquals(20, sent.get())
        assertEquals((1..20).toList(), responses.map { (it["id"] as BigDecimal).toInt() }.sorted())
        responses.forEach { assertEquals(false, it.result()["isError"]) }
    }

    @Test fun `a call overrunning its own limit makes EOF answer the unstarted calls without running them`() {
        val sent = AtomicInteger()
        val release = CountDownLatch(1)
        try {
            val responses = queuedSession(10, callLimitMs = 300) { _, _, _, _ ->
                sent.incrementAndGet()
                release.await()
                CliResponse(0, output = "/saved")
            }
            assertEquals(4, sent.get())
            assertEquals((5..10).toList(), responses.map { (it["id"] as BigDecimal).toInt() }.sorted())
            responses.forEach {
                val error = it["error"] as Map<*, *>
                assertEquals(BigDecimal(-32000), error["code"])
                assertTrue((error["message"] as String).contains("実行していません"))
            }
        } finally {
            release.countDown()
        }
        Thread.sleep(300)
        assertEquals(4, sent.get())
    }

    @Test fun `EOF drains trim preflight and execution within each CLI request limit`() {
        val responses = controlledTrimEof()
        assertEquals((1..10).toList(), responses.map { (it["id"] as BigDecimal).toInt() }.sorted())
        responses.forEach { assertEquals(false, it.result()["isError"]) }
        responses.filter { (it["id"] as BigDecimal).toInt() <= 4 }.forEach {
            assertEquals("/saved-trim", (it.result()["structuredContent"] as Map<*, *>)["directory"])
        }
    }

    @Test fun `EOF still rejects queued calls if trim preflight exceeds its own limit`() {
        assertTrimStageOverrun(CliCommand.TRIM_PREFLIGHT)
    }

    @Test fun `EOF still rejects queued calls if trim execution exceeds its own limit`() {
        assertTrimStageOverrun(CliCommand.TRIM)
    }

    private fun assertTrimStageOverrun(stage: CliCommand) {
        val responses = controlledTrimEof(stage)
        assertEquals((5..10).toList(), responses.map { (it["id"] as BigDecimal).toInt() }.sorted())
        responses.forEach {
            val error = it["error"] as Map<*, *>
            assertEquals(BigDecimal(-32000), error["code"])
            assertTrue((error["message"] as String).contains("実行していません"))
        }
    }

    @Test fun `EOF drains a batch that outlasts one guard while every CLI request stays in time`() {
        val clock = AtomicLong()
        val sent = AtomicInteger()
        val checks = AtomicInteger()
        val release = CountDownLatch(1)
        val completed = CountDownLatch(1)
        val inputThread = Thread.currentThread()
        val output = ByteArrayOutputStream()
        val batch = (1..4).joinToString(",", prefix = "[", postfix = "]\n") { call(it, "get_status") }
        try {
            ReplayMcpServer(batch.byteInputStream(), output, PrintStream(ByteArrayOutputStream()), temporary, temporary,
                { "build" }, send = { _, _, _, _ ->
                    val index = sent.incrementAndGet()
                    clock.set(TimeUnit.SECONDS.toNanos(119L * index))
                    if (index == 4) try {
                        check(release.await(10, TimeUnit.SECONDS))
                    } finally { completed.countDown() }
                    CliResponse(0, "{}")
                }, nanoTime = {
                    clock.get().also { now ->
                        if (Thread.currentThread() == inputThread && now == TimeUnit.SECONDS.toNanos(476) &&
                            checks.incrementAndGet() == 2) release.countDown()
                    }
                }).run()
            val responses = McpJson.parse(output.toString(Charsets.UTF_8).trim()) as List<*>
            assertEquals((1..4).map { BigDecimal(it) }, responses.map { (it as Map<*, *>)["id"] })
            responses.forEach { assertEquals(false, (it as Map<*, *>).result()["isError"]) }
            assertEquals(4, sent.get())
        } finally {
            release.countDown()
            assertTrue(completed.await(10, TimeUnit.SECONDS))
        }
    }

    /** Four controlled in-flight trims hold the workers while six status requests wait behind EOF. */
    private fun controlledTrimEof(stalled: CliCommand? = null): List<Map<*, *>> {
        val clock = AtomicLong()
        val preflights = CountDownLatch(4)
        val executions = CountDownLatch(4)
        val release = CountDownLatch(1)
        val completed = CountDownLatch(4)
        val drainChecks = AtomicInteger()
        val preflightCount = AtomicInteger()
        val executionCount = AtomicInteger()
        val statusCount = AtomicInteger()
        val inputThread = Thread.currentThread()
        val output = ByteArrayOutputStream()
        val lines = (1..10).map { id ->
            if (id <= 4) call(id, "trim_replay", """{"folder":"/saved","from_seconds":0,"to_seconds":1}""")
            else call(id, "get_status")
        }
        val responses = try {
            ReplayMcpServer(lines.joinToString("\n", postfix = "\n").byteInputStream(), output,
                PrintStream(ByteArrayOutputStream()), temporary, temporary.resolve("registry"), { "build" },
                send = { _, _, _, request ->
                    when (request.command) {
                        CliCommand.TRIM_PREFLIGHT -> {
                            preflightCount.incrementAndGet()
                            preflights.countDown()
                            check(preflights.await(10, TimeUnit.SECONDS))
                            if (stalled == CliCommand.TRIM_PREFLIGHT) try {
                                clock.set(TimeUnit.SECONDS.toNanos(141))
                                check(release.await(10, TimeUnit.SECONDS))
                                CliResponse(1, error = "preflight transport timeout")
                            } finally { completed.countDown() }
                            else {
                                clock.set(TimeUnit.SECONDS.toNanos(119))
                                CliResponse(0, """{"fingerprint":"snapshot"}""")
                            }
                        }
                        CliCommand.TRIM -> try {
                            executionCount.incrementAndGet()
                            executions.countDown()
                            check(executions.await(10, TimeUnit.SECONDS))
                            // Both IDE stages take 119s (<120s); their sum exceeds the old 140s EOF guard.
                            clock.set(TimeUnit.SECONDS.toNanos(if (stalled == CliCommand.TRIM) 260 else 238))
                            check(release.await(10, TimeUnit.SECONDS))
                            CliResponse(0, "/saved-trim")
                        } finally { completed.countDown() }
                        CliCommand.STATUS -> { statusCount.incrementAndGet(); CliResponse(0, "{}") }
                        else -> error("unexpected request: ${request.command}")
                    }
                }, nanoTime = {
                    clock.get().also { now ->
                        if (stalled == null && Thread.currentThread() == inputThread && now == TimeUnit.SECONDS.toNanos(238) &&
                            drainChecks.incrementAndGet() == 2) release.countDown()
                    }
                }).run()
            output.toString(Charsets.UTF_8).lines().filter(String::isNotEmpty).map { McpJson.parse(it) as Map<*, *> }
        } finally {
            release.countDown()
            assertTrue(completed.await(10, TimeUnit.SECONDS))
        }
        assertEquals(4, preflightCount.get())
        assertEquals(if (stalled == CliCommand.TRIM_PREFLIGHT) 0 else 4, executionCount.get())
        assertEquals(if (stalled == null) 6 else 0, statusCount.get())
        return responses
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

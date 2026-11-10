package io.github.shinma06.replaybuffer.cli

import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.io.PrintStream
import java.math.BigDecimal
import java.nio.file.Files
import java.nio.file.InvalidPathException
import java.nio.file.Path
import java.util.Base64
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

/**
 * Stdio MCP server (newline-delimited JSON-RPC 2.0) over the CLI's per-request IDE connection.
 *
 * Each tool call is one [CliRequest] with the same contract, exit codes and no-retry rule as the terminal commands.
 * Only protocol messages reach [output]; diagnostics go to [log].
 */
internal class ReplayMcpServer(
    input: InputStream,
    private val output: OutputStream,
    private val log: PrintStream,
    private val defaultProject: Path?,
    private val directory: Path,
    private val build: () -> String,
    private val send: (Path, Path?, String, CliRequest) -> CliResponse = ReplayCli::exchange,
) {
    private class RpcError(val code: Int, message: String) : Exception(message)

    private class Tool(
        val name: String,
        val description: String,
        val properties: Map<String, Any?>,
        val annotations: Map<String, Any?>,
        val call: (Map<String, Any?>) -> Map<String, Any?>,
    )

    private val input = input.buffered()
    private val lock = Any()
    @Volatile private var protocolVersion = LATEST_VERSION
    private var buildIdentity: String? = null
    private val workers = ThreadPoolExecutor(MAX_CONCURRENT_CALLS, MAX_CONCURRENT_CALLS, 10, TimeUnit.SECONDS,
        ArrayBlockingQueue(MAX_QUEUED_CALLS)) { task -> Thread(task, "replay-mcp-call").apply { isDaemon = true } }

    private val tools = listOf(
        Tool("list_projects", "List the Android Studio projects that currently expose an Android Replay Buffer endpoint to this CLI. " +
            "Use a returned `project` as the `project` argument of the other tools when more than one project is open.",
            emptyMap(), readOnly) { listProjects() },
        Tool("get_status", "Read the capture state of the project as JSON without changing anything (same as `replay status`): " +
            "`running` (capture ON), `capture_state`, `device_serial`, `save_state`, `last_save` (`directory` only when completed, " +
            "`retained_path` for a retained failed attempt), `error` and `reason`. ON does not mean frames are being captured; check `capture_state` and `error`.",
            projectOnly, readOnly) { args -> command(args, CliCommand.STATUS) },
        Tool("start_capture", "Turn capture ON for the project (same as the GUI toggle and `replay daemon`). The IDE owns the capture; " +
            "it may wait for a device or a run target. Call get_status afterwards to see whether recording actually started.",
            projectOnly, mapOf("readOnlyHint" to false, "destructiveHint" to false, "idempotentHint" to true, "openWorldHint" to false)) { args ->
            command(args, CliCommand.DAEMON)
        },
        Tool("stop_capture", "Turn capture OFF for the project only (same as `replay stop`). The unsaved buffer and failed save targets are discarded " +
            "exactly as with the GUI; saved folders, the IDE and other projects are untouched.",
            projectOnly, mapOf("readOnlyHint" to false, "destructiveHint" to true, "idempotentHint" to true, "openWorldHint" to false)) { args ->
            command(args, CliCommand.STOP)
        },
        Tool("save_replay", "Save the buffered last seconds of screen video, logcat and timing (same as the GUI save and `replay save`). " +
            "Turns capture ON first if it is OFF, which cannot save until data is buffered. On success returns the completed folder in `directory`. " +
            "If the outcome is unknown (timeout or disconnect), do not retry automatically; check get_status `last_save` first.",
            projectOnly, mapOf("readOnlyHint" to false, "destructiveHint" to false, "idempotentHint" to false, "openWorldHint" to false)) { args ->
            command(args, CliCommand.SAVE)
        },
        Tool("take_screenshot", "Save a PNG of the device screen through the same path as the GUI screenshot button, into the configured " +
            "screenshot folder, and return its `path`. With `include_image` (default true) the PNG is also returned as image content.",
            projectOnly + mapOf("include_image" to mapOf("type" to "boolean", "default" to true,
                "description" to "Also return the saved PNG as MCP image content (omitted above 10 MiB).")),
            mapOf("readOnlyHint" to false, "destructiveHint" to false, "idempotentHint" to false, "openWorldHint" to false)) { args ->
            screenshot(args)
        },
        Tool("get_settings", "Read the project's Android Replay Buffer settings as JSON (the same values as the settings page). " +
            "An empty `output_directory` means no save folder is set (capture only).",
            projectOnly, readOnly) { args -> command(args, CliCommand.SETTINGS) },
        Tool("update_settings", "Change some settings of the project, exactly like applying the settings page: only the given fields change, " +
            "the IDE validates them (folders must already exist and be absolute) and re-resolves the target. `capture_mode` and `video_quality` " +
            "can change only while capture is OFF. Returns the settings after the change.",
            projectOnly + SETTINGS_PROPERTIES,
            mapOf("readOnlyHint" to false, "destructiveHint" to false, "idempotentHint" to true, "openWorldHint" to false)) { args ->
            configure(args)
        },
    ).associateBy { it.name }

    fun run(): Int {
        try {
            while (true) {
                val line = readLine() ?: break
                if (line.isBlank()) continue
                handle(line)
            }
        } catch (failure: java.io.IOException) {
            log.print("MCPの入力を読めません。終了します。\n")
        } finally {
            workers.shutdown()
            // Admitted mutations keep their IDE-side outcome; wait for them instead of abandoning the connection.
            workers.awaitTermination(ReplayCliProtocol.RESULT_TIMEOUT_MS + 10_000L, TimeUnit.MILLISECONDS)
        }
        return 0
    }

    private fun readLine(): String? {
        val bytes = ByteArrayOutputStream()
        var oversized = false
        while (true) {
            val next = input.read()
            if (next < 0) {
                if (bytes.size() == 0 && !oversized) return null
                break
            }
            if (next == '\n'.code) break
            if (bytes.size() >= MAX_MESSAGE_BYTES) oversized = true else bytes.write(next)
        }
        if (oversized) return OVERSIZED
        return bytes.toByteArray().toString(Charsets.UTF_8).removeSuffix("\r")
    }

    private fun handle(line: String) {
        val message = try {
            require(line != OVERSIZED) { "MCPのメッセージが大きすぎます。" }
            McpJson.parse(line)
        } catch (failure: IllegalArgumentException) {
            write(error(null, -32700, failure.message ?: "JSONを解析できません。"))
            return
        }
        if (message is List<*>) {
            if (message.isEmpty()) { write(error(null, -32600, "空のbatchです。")); return }
            submit { message.mapNotNull { dispatch(it) }.takeIf { it.isNotEmpty() }?.let(::write) }
            return
        }
        val obj = message as? Map<*, *>
        if (obj?.get("method") == "tools/call" && "id" in obj) submit(onRejected = { write(error(obj["id"], -32000, "同時に実行できる操作の上限です。完了後に再実行してください。")) }) {
            dispatch(obj)?.let(::write)
        } else dispatch(message)?.let(::write)
    }

    private fun submit(onRejected: () -> Unit = {}, task: () -> Unit) {
        try {
            workers.execute {
                try { task() } catch (failure: Exception) { log.print("MCPの応答を作成できません。\n") }
            }
        } catch (_: RejectedExecutionException) { onRejected() }
    }

    /** Returns the response, or null for a notification. */
    private fun dispatch(message: Any?): Map<String, Any?>? {
        val obj = message as? Map<*, *> ?: return error(null, -32600, "JSON-RPCのrequestはobjectです。")
        val hasId = "id" in obj
        val id = obj["id"]
        if (obj["jsonrpc"] != "2.0" || obj["method"] !is String || hasId && id !is String && id !is BigDecimal) {
            return if (hasId || "method" !in obj) error(if (id is String || id is BigDecimal) id else null, -32600, "JSON-RPC 2.0のrequestではありません。") else null
        }
        if (!hasId) return null
        val params = obj["params"]
        return try {
            if (params != null && params !is Map<*, *>) throw RpcError(-32602, "paramsはobjectで指定してください。")
            @Suppress("UNCHECKED_CAST")
            val result = method(obj["method"] as String, (params as Map<String, Any?>?).orEmpty())
            linkedMapOf("jsonrpc" to "2.0", "id" to id, "result" to result)
        } catch (failure: RpcError) {
            error(id, failure.code, failure.message.orEmpty())
        } catch (_: Exception) {
            error(id, -32603, "MCPの処理に失敗しました。")
        }
    }

    private fun method(name: String, params: Map<String, Any?>): Map<String, Any?> = when (name) {
        "initialize" -> {
            val requested = params["protocolVersion"] as? String
            protocolVersion = if (requested in SUPPORTED_VERSIONS) requested!! else LATEST_VERSION
            linkedMapOf(
                "protocolVersion" to protocolVersion,
                "capabilities" to mapOf("tools" to mapOf("listChanged" to false)),
                "serverInfo" to linkedMapOf("name" to "android-replay-buffer", "title" to "Android Replay Buffer", "version" to pluginVersion()),
                "instructions" to INSTRUCTIONS,
            )
        }
        "ping" -> emptyMap()
        "tools/list" -> mapOf("tools" to tools.values.map { tool ->
            linkedMapOf(
                "name" to tool.name,
                "description" to tool.description,
                "inputSchema" to linkedMapOf("type" to "object", "properties" to tool.properties, "additionalProperties" to false),
                "annotations" to tool.annotations,
            )
        })
        "tools/call" -> {
            val tool = tools[params["name"] as? String] ?: throw RpcError(-32602, "不明なtoolです。tools/listを確認してください。")
            val arguments = params["arguments"] ?: emptyMap<String, Any?>()
            if (arguments !is Map<*, *>) throw RpcError(-32602, "argumentsはobjectで指定してください。")
            @Suppress("UNCHECKED_CAST")
            val args = arguments as Map<String, Any?>
            try {
                args.keys.firstOrNull { it !in tool.properties }?.let { throw IllegalArgumentException("${tool.name}にない引数です: $it") }
                tool.call(args)
            } catch (failure: IllegalArgumentException) {
                failed(2, failure.message ?: "引数を確認してください。")
            }
        }
        else -> throw RpcError(-32601, "対応していないmethodです。")
    }

    private fun project(args: Map<String, Any?>): Path? {
        val value = args["project"] ?: return defaultProject
        require(value is String && value.isNotEmpty()) { "projectには空でないパスを指定してください。" }
        return try { Path.of(value) } catch (_: InvalidPathException) { throw IllegalArgumentException("projectのパスを確認してください。") }
    }

    private fun currentBuild(): String = synchronized(lock) { buildIdentity ?: build().also { buildIdentity = it } }

    private fun exchange(args: Map<String, Any?>, command: CliCommand, payload: String? = null): CliResponse {
        val project = project(args)
        val identity = try { currentBuild() } catch (failure: Exception) {
            return CliResponse(1, error = (failure as? IllegalArgumentException)?.message ?: "配布物のCLIを利用してください。")
        }
        return send(directory, project, identity, CliRequest(command, payload, false, Path.of("").toAbsolutePath().normalize().toString()))
    }

    private fun command(args: Map<String, Any?>, command: CliCommand, payload: String? = null): Map<String, Any?> {
        val response = exchange(args, command, payload)
        if (response.code != 0) return failed(response.code, response.error)
        val parsed = runCatching { McpJson.parse(response.output) }.getOrNull() as? Map<*, *>
        val structured: Map<String, Any?> = when {
            parsed != null -> parsed.entries.associate { (key, value) -> key as String to value }
            command == CliCommand.SAVE -> linkedMapOf("directory" to response.output)
            else -> linkedMapOf("result" to response.output)
        }.let { if (response.error.isEmpty()) it else it + ("message" to response.error) }
        return result(listOf(response.output, response.error).filter(String::isNotEmpty).joinToString("\n"), structured)
    }

    private fun screenshot(args: Map<String, Any?>): Map<String, Any?> {
        val include = args["include_image"] ?: true
        require(include is Boolean) { "include_imageはtrue/falseで指定してください。" }
        val response = exchange(args, CliCommand.SCREENSHOT)
        if (response.code != 0) return failed(response.code, response.error)
        val path = Path.of(response.output)
        val structured = linkedMapOf<String, Any?>("path" to response.output)
        val content = mutableListOf<Map<String, Any?>>(mapOf("type" to "text", "text" to response.output))
        if (include) {
            val image = runCatching {
                if (Files.size(path) <= MAX_IMAGE_BYTES) Files.readAllBytes(path) else null
            }.getOrNull()
            if (image != null) content += linkedMapOf("type" to "image", "data" to Base64.getEncoder().encodeToString(image), "mimeType" to "image/png")
            else content += mapOf("type" to "text", "text" to "画像は10 MiBを超えるか読めないため添付していません。保存したファイルを確認してください。")
        }
        return result(content, structured)
    }

    private fun configure(args: Map<String, Any?>): Map<String, Any?> {
        val changes = args.filterKeys { it != "project" }
        require(changes.isNotEmpty()) { "変更する設定を指定してください。" }
        return command(args, CliCommand.CONFIGURE, McpJson.write(changes))
    }

    private fun listProjects(): Map<String, Any?> {
        val identity = runCatching { currentBuild() }.getOrNull()
        val projects = try { ReplayCliRegistry.live(directory) } catch (failure: Exception) {
            return failed(1, (failure as? IllegalArgumentException ?: failure as? IllegalStateException)?.message
                ?: "CLI接続情報を確認できません。アクセス権を確認してください。")
        }
        val structured = mapOf("projects" to projects.sortedBy { it.project }.map {
            linkedMapOf("project" to it.project, "same_build" to (it.build == identity))
        })
        return result(McpJson.write(structured), structured)
    }

    private fun result(text: String, structured: Map<String, Any?>) = result(listOf(mapOf("type" to "text", "text" to text)), structured)

    private fun result(content: List<Map<String, Any?>>, structured: Map<String, Any?>): Map<String, Any?> = linkedMapOf<String, Any?>(
        "content" to content, "isError" to false,
    ).also { if (protocolVersion >= "2025-06-18") it["structuredContent"] = structured }

    private fun failed(code: Int, message: String): Map<String, Any?> = linkedMapOf(
        "content" to listOf(mapOf("type" to "text", "text" to message.ifEmpty { "操作を完了できません。" } + "\n(exit code $code)")),
        "isError" to true,
    )

    private fun error(id: Any?, code: Int, message: String): Map<String, Any?> =
        linkedMapOf("jsonrpc" to "2.0", "id" to id, "error" to linkedMapOf("code" to code, "message" to message))

    private fun write(message: Any) {
        val bytes = (McpJson.write(message) + "\n").toByteArray(Charsets.UTF_8)
        synchronized(lock) {
            output.write(bytes)
            output.flush()
        }
    }

    companion object {
        const val LATEST_VERSION = "2025-11-25"
        val SUPPORTED_VERSIONS = setOf("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
        private const val MAX_CONCURRENT_CALLS = 4
        private const val MAX_QUEUED_CALLS = 16
        private const val MAX_MESSAGE_BYTES = 1024 * 1024
        private const val MAX_IMAGE_BYTES = 10L * 1024 * 1024
        private const val OVERSIZED = "\u0000oversized"
        private val readOnly = mapOf("readOnlyHint" to true, "destructiveHint" to false, "idempotentHint" to true, "openWorldHint" to false)
        private val projectOnly = mapOf("project" to mapOf("type" to "string",
            "description" to "Absolute path of the open project. Optional when the server was started with --project or exactly one project is open."))
        private val SETTINGS_PROPERTIES: Map<String, Any?> = linkedMapOf(
            "replay_seconds" to mapOf("type" to "integer", "minimum" to 1, "maximum" to 900, "description" to "Seconds kept in the replay buffer."),
            "output_directory" to mapOf("type" to "string", "description" to "Existing absolute folder for saved replays; empty for capture only."),
            "app_selection" to mapOf("type" to "string", "enum" to listOf("AUTOMATIC", "MANUAL"),
                "description" to "AUTOMATIC follows the selected run configuration; MANUAL uses manual_package."),
            "manual_package" to mapOf("type" to "string", "description" to "Application id used when app_selection is MANUAL."),
            "capture_mode" to mapOf("type" to "string", "enum" to listOf("VIDEO_AND_LOGS", "LOG_ONLY"), "description" to "Change only while capture is OFF."),
            "video_quality" to mapOf("type" to "string", "enum" to listOf("STANDARD", "LIGHTWEIGHT"), "description" to "Change only while capture is OFF."),
            "auto_save_on_crash" to mapOf("type" to "boolean", "description" to "Save automatically when the target app crashes."),
            "screenshot_uses_logcat_destination" to mapOf("type" to "boolean",
                "description" to "true saves screenshots where Logcat saves them; false uses screenshot_directory."),
            "screenshot_directory" to mapOf("type" to "string", "description" to "Existing absolute folder used when screenshot_uses_logcat_destination is false."),
        )
        private const val INSTRUCTIONS = "Operates Android Replay Buffer inside an already running Android Studio: the same capture, buffer, save, " +
            "screenshot and settings as its GUI. It never starts the IDE. Call get_status first. A tool error whose text ends with exit code 1 is an " +
            "operation failure or an unknown result; never retry a save automatically, check get_status. Exit code 2 means invalid arguments or settings."

        fun pluginVersion(): String = runCatching {
            ReplayMcpServer::class.java.getResourceAsStream("/META-INF/plugin.xml")?.use { stream ->
                Regex("<version>([^<]{1,64})</version>").find(stream.readNBytes(256 * 1024).toString(Charsets.UTF_8))?.groupValues?.get(1)
            }
        }.getOrNull() ?: "unknown"
    }
}

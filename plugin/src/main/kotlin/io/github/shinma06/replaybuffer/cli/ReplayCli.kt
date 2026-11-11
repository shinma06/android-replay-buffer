package io.github.shinma06.replaybuffer.cli

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.PrintStream
import java.math.BigDecimal
import java.net.InetSocketAddress
import java.net.Socket
import java.nio.file.Path
import kotlin.system.exitProcess

/** A null [command] runs the stdio MCP server, which sends its own commands per tool call. */
internal data class CliArguments(
    val command: CliCommand?,
    val project: Path?,
    /** The JSON object of `settings set`, in the `update_settings` field names. */
    val settings: String? = null,
)

/**
 * Turns `KEY=VALUE` words into the `update_settings` JSON object: integer and boolean fields become JSON numbers and
 * booleans when they parse, everything else stays a string, so the IDE's validation decides exactly as for MCP.
 */
internal fun settingsAssignments(words: List<String>): String {
    require(words.isNotEmpty()) { "変更する設定をKEY=VALUEで指定してください。replay --helpを確認してください。" }
    val changes = linkedMapOf<String, Any?>()
    for (word in words) {
        val key = word.substringBefore('=', "")
        require(key.isNotEmpty()) { "設定はKEY=VALUEで指定してください（例: replay_seconds=60）。" }
        require(key !in changes) { "設定項目が重複しています: $key" }
        val value = word.substringAfter('=')
        changes[key] = when ((ReplayMcpServer.SETTINGS_PROPERTIES[key] as? Map<*, *>)?.get("type")) {
            "integer" -> value.takeIf { Regex("-?[0-9]{1,18}").matches(it) }?.let(::BigDecimal) ?: value
            "boolean" -> when (value) { "true" -> true; "false" -> false; else -> value }
            else -> value
        }
    }
    return McpJson.write(changes).also {
        require(it.toByteArray(Charsets.UTF_8).size <= ReplayCliProtocol.MAX_CONFIG_BYTES) { "設定の変更内容は64 KiB以内で指定してください。" }
    }
}

internal fun parseCliArguments(arguments: List<String>): CliArguments {
    var command: CliCommand? = null
    var mcp = false
    var logsOnly = false
    var project: Path? = null
    val assignments = mutableListOf<String>()
    var index = 0
    while (index < arguments.size) {
        val value = arguments[index++]
        when {
            value == "--project" || value.startsWith("--project=") -> {
                require(project == null) { "--projectは1回だけ指定してください。" }
                val text = if (value == "--project") arguments.getOrNull(index++) else value.substringAfter('=')
                require(!text.isNullOrEmpty()) { "--projectには開いているprojectのパスを指定してください。" }
                project = Path.of(text)
            }
            value == "--logs-only" -> {
                require(!logsOnly) { "--logs-onlyは1回だけ指定してください。" }
                logsOnly = true
            }
            command == CliCommand.SETTINGS && value == "set" -> command = CliCommand.CONFIGURE
            command == CliCommand.CONFIGURE -> assignments += value
            else -> {
                require(command == null && !mcp) { "操作は1つ指定してください。" }
                if (value == "mcp") mcp = true else command = when (value) {
                    "start" -> CliCommand.START
                    "stop" -> CliCommand.STOP
                    "save" -> CliCommand.SAVE
                    "status" -> CliCommand.STATUS
                    "screenshot" -> CliCommand.SCREENSHOT
                    "settings" -> CliCommand.SETTINGS
                    else -> throw IllegalArgumentException("不明な操作・optionです。replay --helpを確認してください。")
                }
            }
        }
    }
    require(!logsOnly || (!mcp && command == CliCommand.SAVE)) { "--logs-onlyはsaveだけに指定できます。" }
    if (mcp) return CliArguments(null, project)
    val selected = requireNotNull(command) { "start/stop/save/status/screenshot/settings/mcpを指定してください。" }
        .let { if (logsOnly) CliCommand.SAVE_LOGS else it }
    return CliArguments(selected, project, if (selected == CliCommand.CONFIGURE) settingsAssignments(assignments) else null)
}

/** Distribution entrypoint for an already running IDE; never starts the IDE or a separate capture process. */
object ReplayCli {
    private const val HELP = """Usage: replay [--project PATH] start|stop|save|status|screenshot
       replay [--project PATH] save --logs-only
       replay [--project PATH] settings
       replay [--project PATH] settings set KEY=VALUE...
       replay [--project PATH] mcp

起動済みAndroid Studioのプラグインを操作するリモコンです。GUI・CLI・MCPは同じ設定・記録・保存処理を使います。
start: 取得をONにして終了します。取得はIDEが続けます。
stop: 対象projectの取得をOFFにします。未保存の記録は破棄します。
save: GUIと同じ保存を実行し、完成フォルダだけをstdoutへ返します。OFFなら取得を開始します。
      記録の蓄積前は保存できません。GUIの状態を確認して再実行してください。
save --logs-only: Logcatツールバーのボタンと同じく、端末ログ・アプリログ・replay.json・README.mdだけを保存します。
      動画の取得中でもMP4は作らず、動画の取得は止めません（replay.jsonのcoverage.videoはEXCLUDED）。
status: 設定を変えずに状態をJSONで表示します。取得ON中は接続中の端末一覧（devices）と取得対象（device_serial）も返します。
screenshot: GUIと同じスクリーンショットを保存し、PNGのパスだけをstdoutへ返します。
settings: 現在の設定をMCPのget_settingsと同じJSONでstdoutへ返します。
settings set: 指定項目だけをMCPのupdate_settingsと同じ項目名・検証で変更し、変更後の設定JSONを返します。
      例: replay settings set replay_seconds=60 auto_save_on_crash=true
      項目: replay_seconds output_directory app_selection manual_package capture_mode video_quality
            auto_save_on_crash screenshot_uses_logcat_destination screenshot_directory tap_display device
      tap_display: OFF=記録のみ、VIDEO=保存する動画にだけタップを描画、DEVICE=端末画面にも表示（取得OFFで変更）。
      show_touches=true/falseは旧項目で、tap_displayのDEVICE/OFFとして扱います。
      device: 取得する端末のserial（statusのdevices[].serial）。空（device=）なら1台だけ接続時に自動選択します。
              複数接続で未選択のときは取得せず選択を待ちます。選んだ端末が切断されても別の端末へは切り替えません。
mcp: 標準入出力のMCPサーバーとして動き、エージェントへ同じ操作を提供します。
--project: 開いているprojectのフォルダ。省略時は接続先が1つだけ必要です。
CLIはIDEに導入済みのプラグインのbinにあり、場所は設定画面からコピーできます。
終了コード: 0=完了、1=操作・接続失敗/結果不明、2=引数・設定値の不正。
"""

    @JvmStatic
    fun main(args: Array<String>) { exitProcess(run(args.toList())) }

    internal fun run(
        arguments: List<String>,
        output: PrintStream = PrintStream(System.out, true, Charsets.UTF_8),
        error: PrintStream = PrintStream(System.err, true, Charsets.UTF_8),
        directory: Path = ReplayCliRegistry.directory(),
        build: () -> String = ReplayCliRegistry::buildIdentity,
    ): Int {
        if (arguments.any { it == "--help" || it == "-h" }) {
            output.print(HELP)
            return 0
        }
        val parsed = try { parseCliArguments(arguments) } catch (failure: Exception) {
            error.line(failure.message ?: "引数を確認してください。")
            return 2
        }
        if (parsed.command == null) {
            return ReplayMcpServer(System.`in`, output, error, parsed.project, directory, build).run()
        }
        val identity = try { build() } catch (failure: Exception) {
            error.line((failure as? IllegalArgumentException ?: failure as? IllegalStateException)?.message
                ?: "CLI接続先を確認できません。IDE/project、パスとアクセス権を確認してください。")
            return 1
        }
        val response = exchange(directory, parsed.project, identity, CliRequest(parsed.command, parsed.settings))
        if (response.output.isNotEmpty()) output.line(response.output)
        if (response.error.isNotEmpty()) error.line(response.error)
        return response.code
    }

    /** Sends one request to the selected project's IDE; never retries once the request may have reached it. */
    internal fun exchange(directory: Path, project: Path?, build: String, request: CliRequest): CliResponse {
        var submitted = false
        return try {
            val endpoint = ReplayCliRegistry.select(directory, project, build)
            Socket().use { socket ->
                socket.connect(InetSocketAddress("127.0.0.1", endpoint.port), ReplayCliProtocol.HANDSHAKE_TIMEOUT_MS)
                socket.soTimeout = ReplayCliProtocol.HANDSHAKE_TIMEOUT_MS
                val input = DataInputStream(socket.getInputStream())
                val wireOutput = DataOutputStream(socket.getOutputStream())
                ReplayCliProtocol.authenticateClient(input, wireOutput, endpoint.token)
                require(ReplayCliProtocol.readText(input) == endpoint.id && ReplayCliProtocol.readText(input) == endpoint.project &&
                    ReplayCliProtocol.readText(input) == endpoint.build) { "CLI接続先が変わりました。操作していません。" }
                socket.soTimeout = ReplayCliProtocol.RESULT_TIMEOUT_MS
                // A partial write can reach the server; subsequent failures must report an unknown outcome.
                submitted = true
                ReplayCliProtocol.writeRequest(wireOutput, request)
                ReplayCliProtocol.readResponse(input)
            }
        } catch (failure: Exception) {
            CliResponse(1, error = if (submitted) "通信が終了したため操作結果を確認できません。自動再試行はしません。GUIの保存結果・状態を確認してください。"
                else (failure as? IllegalArgumentException ?: failure as? IllegalStateException)?.message ?: "CLI接続先を確認できません。IDE/project、パスとアクセス権を確認してください。")
        }
    }
}

/** One LF-terminated line on every OS, unlike println, which uses CRLF on Windows. */
private fun PrintStream.line(text: String?) = print("$text\n")

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

/** Seconds as a decimal string to whole microseconds; fractions below 1 µs are rejected. */
internal fun secondsToUs(text: String, flag: String): Long {
    val seconds = text.toBigDecimalOrNull()
    require(seconds != null && seconds.signum() >= 0 && seconds < BigDecimal("1e13")) { "${flag}には0以上の秒を指定してください。" }
    return try { seconds.movePointRight(6).toBigIntegerExact().longValueExact() }
    catch (_: ArithmeticException) { throw IllegalArgumentException("${flag}にはマイクロ秒精度までの秒を指定してください。") }
}

private fun optionValue(arguments: List<String>, index: Int, value: String, name: String): Pair<String, Int> {
    val text = if (value == name) arguments.getOrNull(index) else value.substringAfter('=')
    require(!text.isNullOrEmpty()) { "${name}には秒を指定してください。" }
    return text to (if (value == name) index + 1 else index)
}

internal fun parseCliArguments(arguments: List<String>): CliArguments {
    var command: CliCommand? = null
    var mcp = false
    var logsOnly = false
    var project: Path? = null
    var folder: String? = null
    var fromUs: Long? = null
    var toUs: Long? = null
    var atUs: Long? = null
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
            command == CliCommand.TRIM && (value == "--from" || value.startsWith("--from=")) -> {
                require(fromUs == null) { "--fromは1回だけ指定してください。" }
                fromUs = optionValue(arguments, index, value, "--from").let { (text, next) -> index = next; secondsToUs(text, "--from") }
            }
            command == CliCommand.TRIM && (value == "--to" || value.startsWith("--to=")) -> {
                require(toUs == null) { "--toは1回だけ指定してください。" }
                toUs = optionValue(arguments, index, value, "--to").let { (text, next) -> index = next; secondsToUs(text, "--to") }
            }
            command == CliCommand.FRAME && (value == "--at" || value.startsWith("--at=")) -> {
                require(atUs == null) { "--atは1回だけ指定してください。" }
                atUs = optionValue(arguments, index, value, "--at").let { (text, next) -> index = next; secondsToUs(text, "--at") }
            }
            (command == CliCommand.TRIM || command == CliCommand.FRAME) && !value.startsWith("--") -> {
                require(folder == null) { "保存フォルダは1つだけ指定してください。" }
                folder = value
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
                    "trim" -> CliCommand.TRIM
                    "frame" -> CliCommand.FRAME
                    else -> throw IllegalArgumentException("不明な操作・optionです。replay --helpを確認してください。")
                }
            }
        }
    }
    require(!logsOnly || (!mcp && command == CliCommand.SAVE)) { "--logs-onlyはsaveだけに指定できます。" }
    if (mcp) return CliArguments(null, project)
    val selected = requireNotNull(command) { "start/stop/save/status/screenshot/settings/trim/frame/mcpを指定してください。" }
        .let { if (logsOnly) CliCommand.SAVE_LOGS else it }
    val payload = when (selected) {
        CliCommand.CONFIGURE -> settingsAssignments(assignments)
        CliCommand.TRIM -> {
            require(folder != null && fromUs != null && toUs != null) {
                "trimには保存フォルダと--from・--to（秒）を指定してください。"
            }
            require(fromUs < toUs) { "--fromは--toより前の秒を指定してください。" }
            McpJson.write(linkedMapOf("folder" to folder, "from_us" to fromUs, "to_us" to toUs))
        }
        CliCommand.FRAME -> {
            require(folder != null && atUs != null) { "frameには保存フォルダと--at（秒）を指定してください。" }
            McpJson.write(linkedMapOf("folder" to folder, "at_us" to atUs))
        }
        else -> null
    }
    return CliArguments(selected, project, payload)
}

/** Distribution entrypoint for an already running IDE; never starts the IDE or a separate capture process. */
object ReplayCli {
    private const val HELP = """Usage: replay [--project PATH] start|stop|save|status|screenshot
       replay [--project PATH] save --logs-only
       replay [--project PATH] trim <保存フォルダ> --from <秒> --to <秒>
       replay [--project PATH] frame <保存フォルダ> --at <秒>
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
trim: 保存済みリプレイの前後を動画内時刻（秒）で切り取り、元を残したまま別フォルダへ保存します。
      成功時は新しいフォルダのパスだけをstdoutへ返します。
      残る表示frameの時計対応が不明なら作成前にstderrへ警告し、不確かな時刻のログを残します。
      事前確認後に元の記録が変われば作成前に拒否します（自動再試行なし）。GUIも同じ判定です。
frame: 保存済みリプレイの指定秒に表示されていた画面を元解像度のPNGで保存します。
      成功時はPNGのパスだけをstdoutへ返します。保存先はスクリーンショットと同じ設定です。
      保存失敗は終了コード1で、理由と保全した出力がある場合はそのパスをstderrへ返します。
      Logcatの保存先を取得できない場合や対応できないframeの復号失敗も、GUIと同じ具体的な理由を返します。
      PNGのテキストmetadataに元のsave_id（arb:source_save_id）・現在のフォルダ名・動画内時刻を記録します。
      Latin-1で表せる値は従来のtEXt、それ以外はUTF-8のiTXtで保持します（キーは共通）。
      t_nsは既知の場合だけ記録します。元のsave_idが欠落・不正なら作成しません。
settings: 現在の設定をMCPのget_settingsと同じJSONでstdoutへ返します。
settings set: 指定項目だけをMCPのupdate_settingsと同じ項目名・検証で変更し、変更後の設定JSONを返します。
      例: replay settings set replay_seconds=60 auto_save_on_crash=true
      項目: replay_seconds output_directory app_selection manual_package capture_mode video_quality
            auto_save_on_crash screenshot_uses_logcat_destination screenshot_directory tap_display
            record_audio device
      tap_display: OFF=記録のみ、VIDEO=保存する動画にだけタップを描画、DEVICE=端末画面にも表示（取得OFFで変更）。
      show_touches=true/falseは旧項目で、tap_displayのDEVICE/OFFとして扱います。
      device: 取得する端末のserial（statusのdevices[].serial）。空（device=）なら1台だけ接続時に自動選択します。
              複数接続で未選択のときは取得せず選択を待ちます。選んだ端末が切断されても別の端末へは切り替えません。
mcp: 標準入出力のMCPサーバーとして動き、エージェントへ同じ操作を提供します。
      trim_replayの事前警告は標準loggingで送り、logging/setLevelに従います（client側の表示保証なし）。
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
        val send = { request: CliRequest -> exchange(directory, parsed.project, identity, request) }
        val response = if (parsed.command == CliCommand.TRIM) trim(checkNotNull(parsed.settings), send) { warning ->
            error.line(warning)
            check(!error.checkError()) { "切り取り前の警告を出力できませんでした。操作していません。" }
        } else send(CliRequest(parsed.command, parsed.settings))
        if (response.output.isNotEmpty()) output.line(response.output)
        if (response.error.isNotEmpty()) error.line(response.error)
        return response.code
    }

    internal fun trim(payload: String, send: (CliRequest) -> CliResponse, warn: (String) -> Unit): CliResponse {
        val response = send(CliRequest(CliCommand.TRIM_PREFLIGHT, payload))
        if (response.code != 0) return response
        return try {
            val plan = McpJson.parse(response.output) as? Map<*, *> ?: error("切り取りの事前確認応答が不正です。")
            val fingerprint = plan["fingerprint"] as? String ?: error("切り取りの事前確認応答が不正です。")
            require(fingerprint.isNotEmpty()) { "切り取りの事前確認応答が不正です。" }
            val warning = plan["warning"]
            require(warning == null || warning is String) { "切り取りの事前確認応答が不正です。" }
            if (warning is String) warn(warning)
            @Suppress("UNCHECKED_CAST")
            val arguments = McpJson.parse(payload) as Map<String, Any?>
            send(CliRequest(CliCommand.TRIM, McpJson.write(arguments + ("fingerprint" to fingerprint))))
        } catch (_: Exception) {
            CliResponse(1, error = "切り取りの事前確認・警告を確認できませんでした。自動再試行しません。")
        }
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

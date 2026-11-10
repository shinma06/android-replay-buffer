package io.github.shinma06.replaybuffer.cli

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.PrintStream
import java.net.InetSocketAddress
import java.net.Socket
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.file.Files
import java.nio.file.Path
import kotlin.system.exitProcess

/** A null [command] runs the stdio MCP server, which sends its own commands per tool call. */
internal data class CliArguments(val command: CliCommand?, val project: Path?, val config: Path?, val foreground: Boolean = false)

internal fun parseCliArguments(rawArguments: List<String>): CliArguments {
    val arguments = rawArguments.flatMap { value ->
        if (value.startsWith("--config=") || value.startsWith("--project=")) listOf(value.substringBefore('='), value.substringAfter('='))
        else listOf(value)
    }
    var project: Path? = null
    var config: Path? = null
    var command: CliCommand? = null
    var mcp = false
    var foreground = false
    var index = 0
    while (index < arguments.size) {
        when (val value = arguments[index++]) {
            "--project", "--config" -> {
                require(index < arguments.size) { "${value}にはパスを指定してください。" }
                val rawPath = arguments[index++]
                require(rawPath.isNotEmpty()) { "${value}には空でないパスを指定してください。" }
                val path = Path.of(rawPath)
                if (value == "--project") {
                    require(project == null) { "--projectの重複指定です。" }
                    project = path
                } else {
                    require(config == null) { "--configの重複指定です。" }
                    config = path
                }
            }
            "--foreground" -> {
                require(!foreground) { "--foregroundの重複指定です。" }
                foreground = true
            }
            "--daemon" -> error("独立daemonは終了しました。起動済みIDEでreplaydを使ってください。")
            else -> {
                require(command == null && !mcp) { "操作は1つ指定してください。" }
                if (value == "mcp") { mcp = true; continue }
                command = when (value) {
                    "save" -> CliCommand.SAVE
                    "status" -> CliCommand.STATUS
                    "stop" -> CliCommand.STOP
                    "daemon" -> CliCommand.DAEMON
                    "screenshot" -> CliCommand.SCREENSHOT
                    else -> throw IllegalArgumentException("不明な操作・optionです。replay --helpを確認してください。")
                }
            }
        }
    }
    if (mcp) {
        require(!foreground) { "--foregroundはdaemonでのみ指定できます。" }
        require(config == null) { "--configはdaemon/saveだけで指定できます。mcpでは設定ツールを使ってください。" }
        return CliArguments(null, project, null)
    }
    val selected = requireNotNull(command) { "save/status/stop/daemon/screenshot/mcpを指定してください。" }
    require(!foreground || selected == CliCommand.DAEMON) { "--foregroundはdaemonでのみ指定できます。" }
    require(config == null || selected == CliCommand.DAEMON || selected == CliCommand.SAVE) {
        "--configはdaemon/saveだけで指定できます。status/stopは設定を変更しません。"
    }
    return CliArguments(selected, project, config, foreground)
}

/** Distribution entrypoint for an already running IDE; never starts a separate capture daemon. */
object ReplayCli {
    private const val HELP = """Usage: replay [--project PATH] [--config FILE] save|status|stop|daemon|screenshot
       replay [--project PATH] mcp
       replayd [--project PATH] [--config FILE] [--foreground]

起動済みAndroid Studioの同じ取得・保存処理を操作します。
daemon/replayd: 取得をONにして終了します（独立常駐・foreground監視はしません）。
save: GUIと同じ保存を実行し、完成フォルダだけをstdoutへ返します。OFFなら取得を開始します。
      記録の蓄積前は保存できません。GUIの状態を確認して再実行してください。
status: 設定を変えずに状態を表示します。stop: 対象projectの取得をOFFにします。
screenshot: GUIと同じスクリーンショットを保存し、PNGのパスだけをstdoutへ返します。
mcp: 標準入出力のMCPサーバーとして動き、エージェントへ状態・保存・スクショ・設定の操作を提供します。
--project: 開いているprojectのフォルダ。省略時は接続先が1つだけ必要です。
--config: 取得OFF時に旧JSON/簡易YAMLの保持秒数・保存先をGUI設定へ明示移行します。
設定・保存形式・常駐動作の移行と復旧は同梱CLI案内を確認してください。
終了コード: 0=完了、1=操作・接続失敗/結果不明、2=引数不正。
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
        if (parsed.foreground) error.line("--foregroundは互換aliasです。取得をONにして終了し、IDEが取得を所有します。")
        val config = try {
            parsed.config?.let { path ->
                if (!Files.exists(path)) throw java.nio.file.NoSuchFileException(path.toString())
                require(Files.isRegularFile(path)) { "設定には通常のファイルを指定してください。" }
                Files.newInputStream(path).use {
                    val bytes = it.readNBytes(ReplayCliProtocol.MAX_CONFIG_BYTES + 1)
                    require(bytes.size <= ReplayCliProtocol.MAX_CONFIG_BYTES) { "設定ファイルは64 KiB以内で指定してください。" }
                    Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
                        .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
                }
            }
        } catch (_: java.nio.charset.CharacterCodingException) {
            error.line("設定ファイルはUTF-8で保存してください。")
            return 2
        } catch (failure: IllegalArgumentException) {
            error.line(failure.message ?: "設定ファイルを確認してください。")
            return 2
        } catch (_: Exception) {
            error.line("設定ファイルを読めません。パスとアクセス権を確認してください。")
            return 1
        }
        if (parsed.command == null) {
            return ReplayMcpServer(System.`in`, output, error, parsed.project, directory, build).run()
        }
        val response = exchange(directory, parsed.project, build(), CliRequest(parsed.command, config,
            parsed.config?.fileName?.toString()?.let { it.endsWith(".yaml") || it.endsWith(".yml") } == true,
            Path.of("").toAbsolutePath().normalize().toString()))
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
                else (failure as? IllegalArgumentException ?: failure as? IllegalStateException)?.message ?: "CLI接続先または設定ファイルを確認できません。IDE/project、パスとアクセス権を確認してください。")
        }
    }
}

/** One LF-terminated line on every OS, unlike println, which uses CRLF on Windows. */
private fun PrintStream.line(text: String?) = print("$text\n")

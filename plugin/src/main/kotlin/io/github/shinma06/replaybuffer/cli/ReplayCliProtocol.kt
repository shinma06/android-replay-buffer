package io.github.shinma06.replaybuffer.cli

import java.io.DataInputStream
import java.io.DataOutputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

internal enum class CliCommand { SAVE, STATUS, STOP, DAEMON }

internal data class CliRequest(
    val command: CliCommand,
    val configText: String? = null,
    val yaml: Boolean = false,
    val workingDirectory: String = "",
)

internal data class CliResponse(val code: Int, val output: String = "", val error: String = "")

/**
 * One mutually authenticated connection carries one request with length-prefixed UTF-8 byte bounds.
 * The client/server own stream closure and deadlines; these blocking serializers do not impose them.
 * A lost response cannot establish whether a mutation ran, so callers must not automatically retry.
 */
internal object ReplayCliProtocol {
    const val VERSION = 1
    const val MAX_CONFIG_BYTES = 64 * 1024
    const val HANDSHAKE_TIMEOUT_MS = 5_000
    const val RESULT_TIMEOUT_MS = 130_000
    private const val MAGIC = 0x41524231
    private const val MAX_TEXT_BYTES = 16 * 1024
    private val random = SecureRandom()

    fun secret(): ByteArray = ByteArray(32).also(random::nextBytes)

    private fun proof(secret: ByteArray, challenge: ByteArray, role: String): ByteArray = Mac.getInstance("HmacSHA256").run {
        init(SecretKeySpec(secret, "HmacSHA256"))
        update(role.toByteArray(Charsets.UTF_8))
        doFinal(challenge)
    }

    fun authenticateClient(input: DataInputStream, output: DataOutputStream, secret: ByteArray) {
        val challenge = ReplayCliProtocol.secret()
        output.writeInt(MAGIC)
        output.writeInt(VERSION)
        output.write(challenge)
        output.flush()
        val serverProof = ByteArray(32).also(input::readFully)
        require(MessageDigest.isEqual(serverProof, proof(secret, challenge, "server"))) { "CLI接続先の所有者を確認できません。" }
        val serverChallenge = ByteArray(32).also(input::readFully)
        output.write(proof(secret, serverChallenge, "client"))
        output.flush()
    }

    fun authenticateServer(input: DataInputStream, output: DataOutputStream, secret: ByteArray) {
        require(input.readInt() == MAGIC && input.readInt() == VERSION) { "CLI通信の形式が異なります。" }
        val challenge = ByteArray(32).also(input::readFully)
        val serverChallenge = ReplayCliProtocol.secret()
        output.write(proof(secret, challenge, "server"))
        output.write(serverChallenge)
        output.flush()
        require(MessageDigest.isEqual(ByteArray(32).also(input::readFully), proof(secret, serverChallenge, "client"))) {
            "CLIの所有者を確認できません。"
        }
    }

    fun readText(input: DataInputStream, limit: Int = MAX_TEXT_BYTES): String {
        val size = input.readInt()
        require(size in 0..limit) { "CLI入力が上限を超えています。" }
        val bytes = ByteArray(size).also(input::readFully)
        return Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
    }

    fun writeText(output: DataOutputStream, value: String, limit: Int = MAX_TEXT_BYTES) {
        val bytes = value.toByteArray(Charsets.UTF_8)
        require(bytes.size <= limit) { "CLI出力が上限を超えています。" }
        output.writeInt(bytes.size)
        output.write(bytes)
    }

    fun readRequest(input: DataInputStream): CliRequest {
        val command = CliCommand.valueOf(readText(input, 16))
        val config = if (input.readBoolean()) readText(input, MAX_CONFIG_BYTES) else null
        val yaml = input.readBoolean()
        val directory = readText(input)
        require(config == null || command == CliCommand.DAEMON || command == CliCommand.SAVE) {
            "--configはdaemon/saveだけで指定できます。status/stopは設定を変更しません。"
        }
        return CliRequest(command, config, yaml, directory)
    }

    fun writeRequest(output: DataOutputStream, request: CliRequest) {
        writeText(output, request.command.name, 16)
        output.writeBoolean(request.configText != null)
        request.configText?.let { writeText(output, it, MAX_CONFIG_BYTES) }
        output.writeBoolean(request.yaml)
        writeText(output, request.workingDirectory)
        output.flush()
    }

    fun readResponse(input: DataInputStream): CliResponse {
        val code = input.readInt()
        require(code in 0..2) { "CLI応答の終了コードが不正です。" }
        return CliResponse(code, readText(input), readText(input))
    }

    fun writeResponse(output: DataOutputStream, response: CliResponse) {
        output.writeInt(response.code)
        writeText(output, response.output)
        writeText(output, response.error)
        output.flush()
    }
}

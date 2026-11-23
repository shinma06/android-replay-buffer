package io.github.shinma06.arb.ipc

import java.io.BufferedInputStream
import java.io.ByteArrayOutputStream
import java.io.Closeable
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.io.RandomAccessFile
import java.net.StandardProtocolFamily
import java.net.UnixDomainSocketAddress
import java.nio.channels.Channels
import java.nio.channels.SocketChannel
import java.nio.charset.StandardCharsets
import java.nio.file.Path

/** Default maximum NDJSON IPC line length (4 MiB), matching `arb-os/src/ipc.rs`. */
const val DEFAULT_MAX_IPC_LINE_BYTES: Int = 4 * 1024 * 1024

/**
 * Bidirectional NDJSON line transport connected to `arbd` over local IPC.
 */
interface LocalIpcTransport : Closeable {
    val endpoint: String

    @Throws(IOException::class)
    fun writeLine(jsonLine: String)

    /**
     * Reads one UTF-8 NDJSON line (stripping trailing `\n` and optional `\r`), or returns `null` on clean EOF.
     */
    @Throws(IOException::class)
    fun readLine(): String?

    companion object {
        @Throws(IOException::class)
        fun connect(
            endpoint: String,
            os: HostOs = HostOs.detect(),
            maxLineBytes: Int = DEFAULT_MAX_IPC_LINE_BYTES,
        ): LocalIpcTransport = when (os) {
            HostOs.MACOS, HostOs.LINUX -> PosixUnixSocketTransport.connect(endpoint, maxLineBytes)
            HostOs.WINDOWS -> WindowsNamedPipeTransport.connect(endpoint, maxLineBytes)
        }
    }
}

internal object NdjsonFraming {
    fun writeFrameLine(out: OutputStream, jsonLine: String, maxLineBytes: Int) {
        val bytes = jsonLine.toByteArray(StandardCharsets.UTF_8)
        if (bytes.size > maxLineBytes) {
            throw IOException("IPC frame length ${bytes.size} exceeds max $maxLineBytes bytes")
        }
        for (b in bytes) {
            if (b == '\n'.code.toByte() || b == '\r'.code.toByte()) {
                throw IllegalArgumentException("NDJSON frame line must not contain embedded CR or LF bytes")
            }
        }
        out.write(bytes)
        out.write('\n'.code)
        out.flush()
    }

    fun readFrameLine(input: InputStream, maxLineBytes: Int): String? {
        val buf = ByteArrayOutputStream(256)
        var anyRead = false
        while (true) {
            val ch = input.read()
            if (ch < 0) {
                if (!anyRead) return null
                break
            }
            anyRead = true
            if (ch == '\n'.code) {
                break
            }
            if (buf.size() >= maxLineBytes) {
                // Drain until newline to keep stream framing aligned.
                var drain = input.read()
                while (drain >= 0 && drain != '\n'.code) {
                    drain = input.read()
                }
                throw IOException("IPC frame exceeded max $maxLineBytes bytes")
            }
            buf.write(ch)
        }
        val raw = buf.toByteArray()
        val end = if (raw.isNotEmpty() && raw[raw.size - 1] == '\r'.code.toByte()) raw.size - 1 else raw.size
        return String(raw, 0, end, StandardCharsets.UTF_8)
    }
}

/**
 * macOS / Linux Unix Domain Socket client using standard Java 16+ NIO (`SocketChannel.open(UNIX)`).
 * Zero external JNI dependencies.
 */
class PosixUnixSocketTransport private constructor(
    override val endpoint: String,
    private val channel: SocketChannel,
    private val input: InputStream,
    private val output: OutputStream,
    private val maxLineBytes: Int,
) : LocalIpcTransport {
    override fun writeLine(jsonLine: String) {
        NdjsonFraming.writeFrameLine(output, jsonLine, maxLineBytes)
    }

    override fun readLine(): String? = NdjsonFraming.readFrameLine(input, maxLineBytes)

    override fun close() {
        channel.close()
    }

    companion object {
        @Throws(IOException::class)
        fun connect(
            endpoint: String,
            maxLineBytes: Int = DEFAULT_MAX_IPC_LINE_BYTES,
        ): PosixUnixSocketTransport {
            val address = UnixDomainSocketAddress.of(Path.of(endpoint))
            val channel = SocketChannel.open(StandardProtocolFamily.UNIX)
            try {
                channel.connect(address)
                val input = BufferedInputStream(Channels.newInputStream(channel), 8192)
                val output = Channels.newOutputStream(channel)
                return PosixUnixSocketTransport(endpoint, channel, input, output, maxLineBytes)
            } catch (e: IOException) {
                runCatching { channel.close() }
                throw e
            }
        }
    }
}

/**
 * Windows Named Pipe client (`\\.\pipe\arb-daemon-*`) using standard JVM `RandomAccessFile`.
 * Retries transient `ERROR_PIPE_BUSY` open errors cleanly without external JNI.
 */
class WindowsNamedPipeTransport private constructor(
    override val endpoint: String,
    private val pipe: RandomAccessFile,
    private val maxLineBytes: Int,
) : LocalIpcTransport {
    private val input = BufferedInputStream(
        object : InputStream() {
            override fun read(): Int = pipe.read()
            override fun read(b: ByteArray, off: Int, len: Int): Int = pipe.read(b, off, len)
        },
        8192,
    )

    private val output = object : OutputStream() {
        override fun write(b: Int) = pipe.write(b)
        override fun write(b: ByteArray, off: Int, len: Int) = pipe.write(b, off, len)
        override fun flush() {
            // Avoid blocking FlushFileBuffers on bidirectional Windows pipes (matches arb-os/src/ipc.rs).
        }
    }

    override fun writeLine(jsonLine: String) {
        NdjsonFraming.writeFrameLine(output, jsonLine, maxLineBytes)
    }

    override fun readLine(): String? = NdjsonFraming.readFrameLine(input, maxLineBytes)

    override fun close() {
        pipe.close()
    }

    companion object {
        private const val PIPE_PREFIX = """\\.\pipe\"""
        private const val MAX_BUSY_RETRIES = 10
        private const val BUSY_RETRY_DELAY_MS = 100L

        @Throws(IOException::class)
        fun connect(
            endpoint: String,
            maxLineBytes: Int = DEFAULT_MAX_IPC_LINE_BYTES,
        ): WindowsNamedPipeTransport {
            if (!endpoint.startsWith(PIPE_PREFIX) || endpoint.length <= PIPE_PREFIX.length) {
                throw IllegalArgumentException("Windows named pipe must start with '$PIPE_PREFIX': $endpoint")
            }
            var lastError: IOException? = null
            for (attempt in 0 until MAX_BUSY_RETRIES) {
                try {
                    val raf = RandomAccessFile(endpoint, "rw")
                    return WindowsNamedPipeTransport(endpoint, raf, maxLineBytes)
                } catch (e: IOException) {
                    lastError = e
                    val msg = e.message.orEmpty().lowercase()
                    if (attempt + 1 < MAX_BUSY_RETRIES && (msg.contains("busy") || msg.contains("231"))) {
                        try {
                            Thread.sleep(BUSY_RETRY_DELAY_MS)
                        } catch (_: InterruptedException) {
                            Thread.currentThread().interrupt()
                            throw e
                        }
                        continue
                    }
                    throw e
                }
            }
            throw lastError ?: IOException("Failed to connect to Windows named pipe: $endpoint")
        }
    }
}

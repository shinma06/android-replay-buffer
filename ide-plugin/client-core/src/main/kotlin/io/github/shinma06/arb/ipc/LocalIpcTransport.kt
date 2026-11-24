package io.github.shinma06.arb.ipc

import java.io.BufferedInputStream
import java.io.ByteArrayOutputStream
import java.io.Closeable
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
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
 * Windows Named Pipe client (`\\.\pipe\arb-daemon-*`).
 *
 * The pipe is opened with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` and its owner SID is
 * verified against the current user **before the first RPC byte is written** (H7), matching the
 * Rust client in `arb-os/src/ipc.rs`. Win32 access goes through [WindowsPipeOpener] /
 * [WindowsPipeSecurityVerifier] so the verification protocol is unit-tested on every OS; production
 * uses the JDK Foreign Function & Memory binding [Win32NamedPipes] (zero JNI / third-party code).
 * Transient `ERROR_PIPE_BUSY` opens are retried.
 */
class WindowsNamedPipeTransport private constructor(
    override val endpoint: String,
    private val pipe: WindowsPipeHandle,
    private val maxLineBytes: Int,
) : LocalIpcTransport {
    private val input = BufferedInputStream(
        object : InputStream() {
            override fun read(): Int {
                val one = ByteArray(1)
                val n = pipe.read(one, 0, 1)
                return if (n <= 0) -1 else one[0].toInt() and 0xff
            }

            override fun read(b: ByteArray, off: Int, len: Int): Int = pipe.read(b, off, len)
        },
        8192,
    )

    private val output = object : OutputStream() {
        override fun write(b: Int) = pipe.write(byteArrayOf(b.toByte()), 0, 1)
        override fun write(b: ByteArray, off: Int, len: Int) = pipe.write(b, off, len)
        override fun flush() {
            // Avoid blocking FlushFileBuffers on bidirectional Windows pipes (matches arb-os/src/ipc.rs).
        }
    }

    override fun writeLine(jsonLine: String) {
        NdjsonFraming.writeFrameLine(BufferedLineOutput(output), jsonLine, maxLineBytes)
    }

    override fun readLine(): String? = NdjsonFraming.readFrameLine(input, maxLineBytes)

    override fun close() {
        pipe.close()
    }

    /** Coalesces one NDJSON frame (payload + `\n`) into a single pipe write. */
    private class BufferedLineOutput(private val sink: OutputStream) : OutputStream() {
        private val buf = ByteArrayOutputStream(256)
        override fun write(b: Int) = buf.write(b)
        override fun write(b: ByteArray, off: Int, len: Int) = buf.write(b, off, len)
        override fun flush() {
            val bytes = buf.toByteArray()
            buf.reset()
            if (bytes.isNotEmpty()) sink.write(bytes, 0, bytes.size)
        }
    }

    companion object {
        private const val PIPE_PREFIX = """\\.\pipe\"""
        private const val MAX_BUSY_RETRIES = 10
        private const val BUSY_RETRY_DELAY_MS = 100L

        @Throws(IOException::class)
        fun connect(
            endpoint: String,
            maxLineBytes: Int = DEFAULT_MAX_IPC_LINE_BYTES,
        ): WindowsNamedPipeTransport = connect(endpoint, maxLineBytes, Win32NamedPipes, Win32NamedPipes.ownerVerifier)

        /**
         * Opens [endpoint] through [opener] and admits it only if [verifier] accepts it; on rejection
         * the handle is closed before a single byte is written.
         */
        @Throws(IOException::class)
        fun connect(
            endpoint: String,
            maxLineBytes: Int,
            opener: WindowsPipeOpener,
            verifier: WindowsPipeSecurityVerifier,
        ): WindowsNamedPipeTransport {
            if (!endpoint.startsWith(PIPE_PREFIX) || endpoint.length <= PIPE_PREFIX.length) {
                throw IllegalArgumentException("Windows named pipe must start with '$PIPE_PREFIX': $endpoint")
            }
            var attempt = 0
            while (true) {
                val handle = try {
                    opener.open(endpoint)
                } catch (busy: WindowsPipeBusyException) {
                    attempt += 1
                    if (attempt >= MAX_BUSY_RETRIES) throw busy
                    try {
                        Thread.sleep(BUSY_RETRY_DELAY_MS)
                    } catch (_: InterruptedException) {
                        Thread.currentThread().interrupt()
                        throw busy
                    }
                    continue
                }
                try {
                    verifier.verify(handle)
                } catch (e: Throwable) {
                    runCatching { handle.close() }
                    if (e is IOException) throw e
                    throw IOException("Windows named pipe verification failed for $endpoint: $e", e)
                }
                return WindowsNamedPipeTransport(endpoint, handle, maxLineBytes)
            }
        }
    }
}

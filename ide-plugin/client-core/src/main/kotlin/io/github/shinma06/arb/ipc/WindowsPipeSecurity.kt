package io.github.shinma06.arb.ipc

import java.io.Closeable
import java.io.IOException

/**
 * One open Windows Named Pipe client handle.
 *
 * Production handles come from [Win32NamedPipes] (opened with
 * `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, exactly like `arb-os/src/ipc.rs`); tests inject
 * fakes so the owner-verification protocol is exercised on every OS.
 */
interface WindowsPipeHandle : Closeable {
    /** Raw SID bytes of the pipe object's security-descriptor owner (the server process user). */
    @Throws(IOException::class)
    fun serverOwnerSid(): ByteArray

    /** Reads up to [len] bytes, returning `-1` on a clean end of stream (broken pipe). */
    @Throws(IOException::class)
    fun read(b: ByteArray, off: Int, len: Int): Int

    /** Writes all [len] bytes. */
    @Throws(IOException::class)
    fun write(b: ByteArray, off: Int, len: Int)
}

/** Opens client handles to a Windows Named Pipe path (`\\.\pipe\...`). */
fun interface WindowsPipeOpener {
    /** @throws WindowsPipeBusyException when every server instance is busy (`ERROR_PIPE_BUSY`). */
    @Throws(IOException::class)
    fun open(path: String): WindowsPipeHandle
}

/** Thrown by [WindowsPipeOpener.open] for the transient `ERROR_PIPE_BUSY` condition. */
class WindowsPipeBusyException(message: String) : IOException(message)

/**
 * Verifies a freshly opened pipe really belongs to the expected server **before the first RPC byte
 * is sent** (H7). Implementations throw to reject the connection; the transport then closes the
 * handle without writing anything.
 */
fun interface WindowsPipeSecurityVerifier {
    @Throws(IOException::class)
    fun verify(handle: WindowsPipeHandle)
}

/** Thrown when a Windows Named Pipe is owned by a different user than the current one. */
class WindowsPipeOwnerMismatchException(message: String) : IOException(message)

/**
 * Accepts a pipe only when its security-descriptor owner SID equals the current user's SID — the
 * same rule as `verify_pipe_server_owned_by_current_user` in `arb-os/src/ipc.rs` (`arbd` explicitly
 * sets its token user as the pipe owner). A same-named pipe squatted by another account is refused.
 */
class OwnerSidPipeVerifier(
    private val currentUserSid: () -> ByteArray,
) : WindowsPipeSecurityVerifier {
    override fun verify(handle: WindowsPipeHandle) {
        val owner = handle.serverOwnerSid()
        val current = currentUserSid()
        if (owner.isEmpty() || current.isEmpty() || !owner.contentEquals(current)) {
            throw WindowsPipeOwnerMismatchException(
                "Refusing Windows named pipe: server owner SID ${formatSid(owner)} " +
                    "does not match the current user SID ${formatSid(current)}",
            )
        }
    }

    companion object {
        /** Renders binary SID bytes as `S-R-I-S...` for diagnostics (best effort). */
        fun formatSid(sid: ByteArray): String {
            if (sid.size < 8) return "<invalid>"
            val revision = sid[0].toInt() and 0xff
            val subCount = sid[1].toInt() and 0xff
            var authority = 0L
            for (i in 2 until 8) {
                authority = (authority shl 8) or (sid[i].toLong() and 0xff)
            }
            val sb = StringBuilder("S-").append(revision).append('-').append(authority)
            for (i in 0 until subCount) {
                val at = 8 + i * 4
                if (at + 4 > sid.size) return "<invalid>"
                val sub = (sid[at].toLong() and 0xff) or
                    ((sid[at + 1].toLong() and 0xff) shl 8) or
                    ((sid[at + 2].toLong() and 0xff) shl 16) or
                    ((sid[at + 3].toLong() and 0xff) shl 24)
                sb.append('-').append(sub)
            }
            return sb.toString()
        }
    }
}

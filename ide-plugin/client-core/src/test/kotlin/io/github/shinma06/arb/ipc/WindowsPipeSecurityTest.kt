package io.github.shinma06.arb.ipc

import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.charset.StandardCharsets
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * H7: the Windows Named Pipe transport must verify the pipe owner SID before sending any RPC byte.
 * The Win32 calls are abstracted behind [WindowsPipeOpener] / [WindowsPipeHandle], so this protocol
 * runs on every OS; the real FFM binding is exercised end-to-end by the Windows CI
 * (`ArbdRealIntegrationTest` connects to a real `arbd` pipe through [LocalIpcTransport.connect]).
 */
class WindowsPipeSecurityTest {
    private val endpoint = """\\.\pipe\arb-daemon-test"""
    private val userSid = sid(21, 1111, 2222, 3333, 1001)
    private val otherSid = sid(21, 1111, 2222, 3333, 1002)

    private fun sid(vararg subAuthorities: Long): ByteArray {
        val out = ByteArrayOutputStream()
        out.write(1) // revision
        out.write(subAuthorities.size)
        out.write(byteArrayOf(0, 0, 0, 0, 0, 5)) // SECURITY_NT_AUTHORITY
        for (sub in subAuthorities) {
            for (shift in 0 until 4) out.write(((sub shr (8 * shift)) and 0xff).toInt())
        }
        return out.toByteArray()
    }

    private class FakePipe(private val owner: ByteArray, reply: String = "") : WindowsPipeHandle {
        val written = ByteArrayOutputStream()
        var ownerQueries = 0
        var closed = false
        private val inbound = reply.toByteArray(StandardCharsets.UTF_8)
        private var readPos = 0

        override fun serverOwnerSid(): ByteArray {
            ownerQueries += 1
            return owner
        }

        override fun read(b: ByteArray, off: Int, len: Int): Int {
            if (readPos >= inbound.size) return -1
            val n = minOf(len, inbound.size - readPos)
            System.arraycopy(inbound, readPos, b, off, n)
            readPos += n
            return n
        }

        override fun write(b: ByteArray, off: Int, len: Int) {
            check(!closed) { "write after close" }
            written.write(b, off, len)
        }

        override fun close() {
            closed = true
        }
    }

    @Test
    fun ownerSidMismatchClosesHandleAndSendsZeroRpcBytes() {
        val squatter = FakePipe(owner = otherSid)
        val error = assertFailsWith<WindowsPipeOwnerMismatchException> {
            WindowsNamedPipeTransport.connect(
                endpoint,
                DEFAULT_MAX_IPC_LINE_BYTES,
                opener = { squatter },
                verifier = OwnerSidPipeVerifier { userSid },
            )
        }
        assertTrue(error.message.orEmpty().contains("S-1-5-21-1111-2222-3333-1002"), error.message)
        assertEquals(1, squatter.ownerQueries, "owner must be checked before use")
        assertTrue(squatter.closed, "rejected pipe handle must be closed immediately")
        assertEquals(0, squatter.written.size(), "zero RPC bytes may reach an unverified pipe")
    }

    @Test
    fun verifierFailureFailsClosedWithoutWriting() {
        val pipe = FakePipe(owner = userSid)
        assertFailsWith<IOException> {
            WindowsNamedPipeTransport.connect(
                endpoint,
                DEFAULT_MAX_IPC_LINE_BYTES,
                opener = { pipe },
                verifier = { throw IllegalStateException("GetKernelObjectSecurity unavailable") },
            )
        }
        assertTrue(pipe.closed, "handle must be closed when verification cannot complete")
        assertEquals(0, pipe.written.size(), "no bytes when verification cannot complete")
    }

    @Test
    fun matchingOwnerSidAdmitsPipeAndFramesNdjson() {
        val pipe = FakePipe(owner = userSid.copyOf(), reply = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\r\n")
        val transport = WindowsNamedPipeTransport.connect(
            endpoint,
            DEFAULT_MAX_IPC_LINE_BYTES,
            opener = { pipe },
            verifier = OwnerSidPipeVerifier { userSid },
        )
        assertEquals(0, pipe.written.size(), "verification itself writes nothing")
        transport.writeLine("""{"jsonrpc":"2.0","id":1,"method":"system.hello"}""")
        assertContentEquals(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"system.hello\"}\n".toByteArray(StandardCharsets.UTF_8),
            pipe.written.toByteArray(),
        )
        assertEquals("""{"jsonrpc":"2.0","id":1,"result":{}}""", transport.readLine())
        assertEquals(null, transport.readLine(), "clean EOF after the reply")
        transport.close()
        assertTrue(pipe.closed)
    }

    @Test
    fun busyPipeIsRetriedBeforeVerification() {
        var opens = 0
        val pipe = FakePipe(owner = userSid)
        val transport = WindowsNamedPipeTransport.connect(
            endpoint,
            DEFAULT_MAX_IPC_LINE_BYTES,
            opener = {
                opens += 1
                if (opens < 3) throw WindowsPipeBusyException("busy")
                pipe
            },
            verifier = OwnerSidPipeVerifier { userSid },
        )
        assertEquals(3, opens, "ERROR_PIPE_BUSY must be retried")
        assertEquals(1, pipe.ownerQueries)
        transport.close()
    }

    @Test
    fun emptyOrMalformedSidsAreNeverTreatedAsMatching() {
        val verifier = OwnerSidPipeVerifier { ByteArray(0) }
        assertFailsWith<WindowsPipeOwnerMismatchException> { verifier.verify(FakePipe(owner = ByteArray(0))) }
        assertEquals("<invalid>", OwnerSidPipeVerifier.formatSid(byteArrayOf(1, 2)))
        assertEquals("S-1-5-21-1111-2222-3333-1001", OwnerSidPipeVerifier.formatSid(userSid))
    }

    @Test
    fun nonPipeEndpointIsRejectedBeforeOpening() {
        var opened = false
        assertFailsWith<IllegalArgumentException> {
            WindowsNamedPipeTransport.connect(
                """C:\temp\arbd.sock""",
                DEFAULT_MAX_IPC_LINE_BYTES,
                opener = {
                    opened = true
                    FakePipe(owner = userSid)
                },
                verifier = OwnerSidPipeVerifier { userSid },
            )
        }
        assertFalse(opened)
    }

    @Test
    fun interruptedCloserReleasesIdleHandleAndPreservesInterrupt() {
        var released = false
        Thread.currentThread().interrupt()
        try {
            closeAfterPipeIo(java.util.concurrent.locks.ReentrantReadWriteLock(), {}, { released = true })
            assertTrue(released)
            assertTrue(Thread.currentThread().isInterrupted)
        } finally {
            Thread.interrupted()
        }
    }

    /** Windows-only facility: the real FFM binding must resolve the process token user SID. */
    @Test
    fun win32CurrentUserSidIsAValidSidOnWindows() {
        assumeTrue(HostOs.detect() == HostOs.WINDOWS, "Win32 token SID lookup is a Windows-only facility")
        val sid = Win32NamedPipes.currentUserSid()
        assertEquals(1, sid[0].toInt(), "SID revision must be 1")
        assertEquals(8 + 4 * (sid[1].toInt() and 0xff), sid.size, "SID length must match its sub-authority count")
        assertTrue(OwnerSidPipeVerifier.formatSid(sid).startsWith("S-1-"))
    }
}

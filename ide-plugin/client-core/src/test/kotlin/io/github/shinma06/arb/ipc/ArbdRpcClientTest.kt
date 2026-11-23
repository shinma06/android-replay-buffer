package io.github.shinma06.arb.ipc

import com.google.gson.JsonObject
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

/** In-memory [LocalIpcTransport] that replays scripted response lines. */
private class FakeTransport(
    responses: List<String?>,
    private val hangOnRead: Boolean = false,
) : LocalIpcTransport {
    override val endpoint: String = "fake://arb-test"
    private val queue = ArrayDeque(responses)
    val written = mutableListOf<String>()
    var closed = false
        private set
    val readStarted = CountDownLatch(1)

    override fun writeLine(jsonLine: String) {
        written.add(jsonLine)
    }

    override fun readLine(): String? {
        readStarted.countDown()
        if (hangOnRead) {
            // A hung daemon: never respond so the bounded read timeout must fire.
            Thread.sleep(60_000)
            return null
        }
        return queue.removeFirstOrNull()
    }

    override fun close() {
        closed = true
    }
}

/** R4 S5/C5: strict response-id matching and bounded read timeout on the Kotlin RPC client. */
class ArbdRpcClientTest {
    private fun okResponse(id: Long, result: String = "{}"): String =
        """{"jsonrpc":"2.0","id":$id,"result":$result}"""

    @Test
    fun `response with mismatched id is rejected`() {
        val transport = FakeTransport(listOf(okResponse(999)))
        val client = ArbdRpcClient(transport)
        val err = assertFailsWith<IOException> {
            client.callRaw("system.status", JsonObject())
        }
        assertTrue(
            err.message.orEmpty().contains("id mismatch"),
            "expected id-mismatch error, got: ${err.message}",
        )
    }

    @Test
    fun `response with missing id is rejected`() {
        val transport = FakeTransport(listOf("""{"jsonrpc":"2.0","result":{}}"""))
        val client = ArbdRpcClient(transport)
        assertFailsWith<IOException> {
            client.callRaw("system.status", JsonObject())
        }
    }

    @Test
    fun `matching id returns parsed result`() {
        val transport = FakeTransport(listOf(okResponse(1, """{"ok":true}""")))
        val client = ArbdRpcClient(transport)
        val result = client.callRaw("system.status", JsonObject())
        assertTrue(result.isJsonObject)
        assertEquals(true, result.asJsonObject.get("ok").asBoolean)
        assertEquals(1, transport.written.size)
    }

    @Test
    fun `read timeout closes transport and raises io error`() {
        val transport = FakeTransport(emptyList(), hangOnRead = true)
        val client = ArbdRpcClient(transport, readTimeoutMs = 100)
        val err = assertFailsWith<IOException> {
            client.callRaw("system.status", JsonObject())
        }
        assertTrue(
            err.message.orEmpty().contains("timed out"),
            "expected timeout error, got: ${err.message}",
        )
        assertTrue(transport.readStarted.await(2, TimeUnit.SECONDS))
        // The transport must be closed on timeout so a late response can never desync
        // the framing of the next call (R4 S5/C5).
        assertTrue(transport.closed, "transport must be closed after read timeout")
        client.close()
    }
}

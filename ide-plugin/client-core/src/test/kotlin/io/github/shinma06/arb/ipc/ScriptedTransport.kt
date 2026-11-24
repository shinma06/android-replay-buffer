package io.github.shinma06.arb.ipc

import com.google.gson.JsonParser
import java.io.IOException
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue

/**
 * In-memory [LocalIpcTransport] answering each JSON-RPC request from [respond]: a `result` JSON
 * string, or an `error` via [ScriptedTransport.Error], or a transport failure via
 * [ScriptedTransport.Drop] (the request was written, the response is lost). Every written method
 * is recorded in [sent].
 */
class ScriptedTransport(
    private val respond: (method: String) -> Any,
) : LocalIpcTransport {
    data class Error(val code: Int, val message: String)

    /** The request reached the peer, but the connection breaks before the response arrives. */
    data object Drop

    override val endpoint: String = "scripted://arbd"
    val sent = CopyOnWriteArrayList<String>()
    private val responses = LinkedBlockingQueue<Any>()

    @Volatile
    var closed = false
        private set

    override fun writeLine(jsonLine: String) {
        if (closed) throw IOException("scripted transport closed")
        val obj = JsonParser.parseString(jsonLine).asJsonObject
        val id = obj.get("id").asLong
        val method = obj.get("method").asString
        sent += method
        responses += when (val r = respond(method)) {
            Drop -> Drop
            is Error -> """{"jsonrpc":"2.0","id":$id,"error":{"code":${r.code},"message":"${r.message}"}}"""
            else -> """{"jsonrpc":"2.0","id":$id,"result":$r}"""
        }
    }

    override fun readLine(): String? {
        val next = responses.take()
        if (next == Drop) {
            closed = true
            throw IOException("scripted transport dropped the response")
        }
        return next as String
    }

    override fun close() {
        closed = true
    }
}

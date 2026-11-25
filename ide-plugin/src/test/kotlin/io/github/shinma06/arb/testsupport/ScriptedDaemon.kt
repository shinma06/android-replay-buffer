package io.github.shinma06.arb.testsupport

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.ipc.LocalIpcTransport
import io.github.shinma06.arb.shm.ShmFrameRingReader
import io.github.shinma06.arb.shm.ShmPixelFormat
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.atomic.AtomicBoolean

/** Handler result prefix: the transport answers with a JSON-RPC `error` object (a business error). */
const val RPC_ERROR_PREFIX: String = "__rpc_error__:"

fun rpcError(message: String): String = "$RPC_ERROR_PREFIX$message"

/** A handler returning this simulates a transport that died mid-call (`IOException` on write). */
class TransportDown(message: String = "connection reset") : java.io.IOException(message)

/** One recorded request. */
class RecordedRpc(val method: String, val params: JsonObject)

/**
 * In-memory JSON-RPC transport driven by [handler]: a result JSON string, [rpcError] for a
 * business error, or a thrown [TransportDown] for a dead connection. Every request is recorded.
 */
class ScriptedTransport(
    val calls: CopyOnWriteArrayList<RecordedRpc> = CopyOnWriteArrayList(),
    private val handler: (method: String, params: JsonObject) -> String,
) : LocalIpcTransport {
    override val endpoint: String = "mock://scripted"
    val closed = AtomicBoolean(false)
    private val responses = LinkedBlockingQueue<String>()

    fun newConnection() = ArbdRpcClient(ScriptedTransport(calls, handler))

    fun count(method: String): Int = calls.count { it.method == method }

    override fun writeLine(jsonLine: String) {
        check(!closed.get()) { "transport closed" }
        val obj = JsonParser.parseString(jsonLine).asJsonObject
        val id = obj.get("id").asLong
        val method = obj.get("method").asString
        val params = obj.get("params")?.takeIf { it.isJsonObject }?.asJsonObject ?: JsonObject()
        calls += RecordedRpc(method, params)
        val result = handler(method, params)
        if (result.startsWith(RPC_ERROR_PREFIX)) {
            val message = result.removePrefix(RPC_ERROR_PREFIX)
            responses.put("""{"jsonrpc":"2.0","id":$id,"error":{"code":-32000,"message":"$message"}}""")
        } else {
            responses.put("""{"jsonrpc":"2.0","id":$id,"result":$result}""")
        }
    }

    override fun readLine(): String? = responses.take()

    override fun close() {
        closed.set(true)
    }
}

/** Polls [check] up to [timeoutMs], pumping nothing: for state published directly by workers. */
fun awaitTrue(msg: () -> String = { "condition timed out" }, timeoutMs: Long = 10_000L, check: () -> Boolean) {
    val deadline = System.currentTimeMillis() + timeoutMs
    while (System.currentTimeMillis() < deadline) {
        if (check()) return
        Thread.sleep(5L)
    }
    check(check()) { msg() }
}

/** A valid triple-buffered ARB SHM file with a 2x2 RGBA8 frame already published in slot 0. */
fun writeValidShmFile(path: Path, frameIndex: Long = 0L): Path {
    val slotCapacity = 64
    val alignedStride = ShmFrameRingReader.alignUp(slotCapacity, ShmFrameRingReader.CACHE_LINE_BYTES)
    val totalSize = ShmFrameRingReader.HEADER_REGION_BYTES + alignedStride * ShmFrameRingReader.SHM_SLOT_COUNT
    val buf = ByteBuffer.allocate(totalSize).order(ByteOrder.LITTLE_ENDIAN)
    buf.position(0)
    buf.put(ShmFrameRingReader.RING_MAGIC)
    buf.putInt(8, ShmFrameRingReader.RING_VERSION)
    buf.putInt(12, ShmFrameRingReader.SHM_SLOT_COUNT)
    buf.putLong(16, slotCapacity.toLong())
    buf.putInt(24, 0)
    buf.putLong(32, 1L)
    val slotBase = ShmFrameRingReader.RING_HEADER_BYTES
    buf.putLong(slotBase + 0, 2L)
    buf.putLong(slotBase + 8, 1L)
    buf.putLong(slotBase + 16, frameIndex)
    buf.putLong(slotBase + 24, 10_000_000_000L)
    buf.putInt(slotBase + 32, 2)
    buf.putInt(slotBase + 36, 2)
    buf.putInt(slotBase + 40, 8)
    buf.putInt(slotBase + 44, ShmPixelFormat.RGBA8.wire)
    buf.putInt(slotBase + 48, 16)
    buf.putInt(slotBase + 52, 0)
    val payloadOffset = ShmFrameRingReader.HEADER_REGION_BYTES
    for (i in 0 until 16) buf.put(payloadOffset + i, 0x7F.toByte())
    Files.write(path, buf.array())
    return path
}

/** `bundle.open` result for a video bundle with [frameCount] frames over [startNs]..[endNs]. */
fun bundleOpenResultJson(bundleId: String, frameCount: Int, startNs: Long, endNs: Long): String =
    """{"bundle_id":"$bundleId","path":"/tmp/$bundleId","format":"v2","is_legacy":false,"lease_id":"lease-$bundleId",""" +
        """"summary":{"bundle_id":"$bundleId","path":"/tmp/$bundleId","format":"v2","is_legacy":false,""" +
        """"provenance":{"kind":"original"},"streams":{"video":{"status":"present","frame_count":$frameCount,""" +
        """"tail_may_be_incomplete":false,"semantics":"video_encoder_input_estimate"}},""" +
        """"unanchored_summary":{"included_count":0,"truncated_count":0},""" +
        """"epochs":[{"boot_id":"boot-1","start_boottime_ns":$startNs,"end_boottime_ns":$endNs}],"epoch_frame_ranges":[],""" +
        """"frame_count":$frameCount,"keyframe_count":2,"log_main_count":0,"log_unanchored_count":0,""" +
        """"tap_main_count":0,"tap_unanchored_count":0,"discontinuity_count":0,"video_path":"/tmp/$bundleId/video.mp4"}}"""

/** `timeline.summary` result (the bare summary object) for catalog meta fetches. */
fun timelineSummaryJson(bundleId: String): String =
    """{"bundle_id":"$bundleId","path":"/x/$bundleId","format":"v2","is_legacy":false,"bundle_kind":"full",""" +
        """"provenance":{"kind":"original"},"streams":{},"unanchored_summary":{"included_count":0,"truncated_count":0},""" +
        """"epochs":[{"boot_id":"b","start_boottime_ns":0,"end_boottime_ns":12000000000}],"frame_count":360,"keyframe_count":4,""" +
        """"log_main_count":0,"log_unanchored_count":0,"tap_main_count":0,"tap_unanchored_count":0,"discontinuity_count":0}"""

fun crashesJson(vararg sourceSeq: Long): String =
    """{"crashes":[""" + sourceSeq.joinToString(",") {
        """{"boottime_ns":5000000000,"frame_index_hint":10,"process_name":"app","package_name":"com.example","pid":1,""" +
            """"exception_class":"java.lang.RuntimeException","message":"boom","source_log_seq":$it}"""
    } + "]}"

/** One main-bucket anchored log row as `timeline.query_logs` returns it. */
fun logRowJson(seq: Long, tsNs: Long = 1_000L + seq): String =
    """{"seq":$seq,"pc_ingest_cont_ns":$tsNs,"alignment":{"certainty":"anchored","boot_id":"boot-1","boottime_ns":$tsNs,""" +
        """"half_width_ns":1,"semantics":"logcat_device_read_boottime"},"bucket":"main","level":"I","tag":"T","message":"m$seq"}"""

const val STATUS_JSON: String =
    """{"pid":77,"uptime_ms":10,"capture":{"active":false,"connected":false,"storage_pressure_stopped":false,"capture_mode":"video_and_logs","video_frames":0,"audio_frames":0,"log_records":0,"tap_records":0,"discontinuities":0,"static_hold_events":0},"ring_chunk_count":0,"ring_total_bytes":0,"ring_pinned_chunk_count":0,"buffered_secs":0,"active_viewports":0,"active_save_jobs":0,"recovery":{}}"""

const val CONFIG_JSON: String =
    """{"config":{"config_version":1,"save_window_secs":300,"ring_budget_bytes":1610612736,"min_free_disk_bytes":3221225472,"video_max_size":1920,"video_max_fps":30,"audio_enabled":true,"capture_mode":"video_and_logs","crash_autosave_enabled":true,"crash_autosave_cooldown_secs":15,"ide_default_save_action":"save_and_open"},"config_path":"/tmp/config.json"}"""

fun bundleEntryJson(id: String, state: String = "active"): String =
    """{"bundle_id":"$id","path":"/data/bundles/$id","format_version":"v2","state":"$state","created_at_utc":"2026-11-25T00:00:00Z","active_leases":0,"disk_state":"present"}"""

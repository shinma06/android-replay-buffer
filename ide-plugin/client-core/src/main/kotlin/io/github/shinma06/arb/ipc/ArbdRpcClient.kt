package io.github.shinma06.arb.ipc

import com.google.gson.GsonBuilder
import com.google.gson.JsonElement
import com.google.gson.JsonNull
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleGcResponse
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.BundleListResponse
import io.github.shinma06.arb.model.BundleOpenResponse
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.CaptureStartResponse
import io.github.shinma06.arb.model.CaptureStopResponse
import io.github.shinma06.arb.model.ConfigResponse
import io.github.shinma06.arb.model.DeviceListResponse
import io.github.shinma06.arb.model.EditSessionEnvelope
import io.github.shinma06.arb.model.EditSessionView
import io.github.shinma06.arb.model.EditUpdateSessionParams
import io.github.shinma06.arb.model.ExportPngParams
import io.github.shinma06.arb.model.ExportPngResponse
import io.github.shinma06.arb.model.ExportShareMp4Params
import io.github.shinma06.arb.model.ExportShareMp4Response
import io.github.shinma06.arb.model.ExportTrimParams
import io.github.shinma06.arb.model.ExportTrimResponse
import io.github.shinma06.arb.model.IPC_PROTOCOL_VERSION
import io.github.shinma06.arb.model.JSONRPC_VERSION
import io.github.shinma06.arb.model.LeaseCloseResponse
import io.github.shinma06.arb.model.LeaseHolderKind
import io.github.shinma06.arb.model.LeaseRenewResponse
import io.github.shinma06.arb.model.MediaCloseViewportResponse
import io.github.shinma06.arb.model.MediaDecodeAudioPcmParams
import io.github.shinma06.arb.model.MediaDecodeAudioPcmResponse
import io.github.shinma06.arb.model.MediaFilmstripResponse
import io.github.shinma06.arb.model.MediaOpenViewportResponse
import io.github.shinma06.arb.model.MediaScrubResponse
import io.github.shinma06.arb.model.MediaSeekFrameResponse
import io.github.shinma06.arb.model.ProtocolJson
import io.github.shinma06.arb.model.QueryBucket
import io.github.shinma06.arb.model.SaveJobView
import io.github.shinma06.arb.model.SaveListJobsResponse
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveOutcomeResponse
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.SaveTriggerParams
import io.github.shinma06.arb.model.ScreenshotResult
import io.github.shinma06.arb.model.ShutdownResponse
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TimelineCrashesResponse
import io.github.shinma06.arb.model.TimelineDiscontinuitiesResponse
import io.github.shinma06.arb.model.TimelineLogsResponse
import io.github.shinma06.arb.model.TimelineScreensResponse
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.TimelineTapsResponse
import java.io.Closeable
import java.io.IOException
import java.util.concurrent.Callable
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicLong

/** Default bounded read timeout for a single JSON-RPC response (30 s, R4 S5/C5). */
const val DEFAULT_READ_TIMEOUT_MS: Long = 30_000L

/** Generous per-call read timeout for long-running exports & PCM audio decodes (120 s, T-6A). */
const val LONG_RUNNING_READ_TIMEOUT_MS: Long = 120_000L

/**
 * Transport-level IPC failure tagged with the JSON-RPC `method` that was in flight, so the
 * reconnect/retry policy can distinguish idempotent reads from mutating calls (R4 S4).
 */
class ArbdIoException(
    val rpcMethod: String,
    cause: Throwable,
) : IOException("arbd IPC transport failed for '$rpcMethod': ${cause.message}", cause)

/**
 * Typed NDJSON JSON-RPC 2.0 client covering all `arbd` control-plane methods (`SPEC-03`).
 * Thread-safe on a single persistent local IPC connection.
 */
class ArbdRpcClient(
    private val transport: LocalIpcTransport,
    private val readTimeoutMs: Long = DEFAULT_READ_TIMEOUT_MS,
    private val longRunningReadTimeoutMs: Long = LONG_RUNNING_READ_TIMEOUT_MS,
) : Closeable {
    private val gson = ProtocolJson.gson
    // Preserves explicit JsonNull entries placed intentionally by adapters (e.g. OutputDirPatch.Clear)
    // while normal DTOs serialized via ProtocolJson.gson.toJsonTree(...) omit null properties.
    private val envelopeGson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
    private val nextId = AtomicLong(1L)
    private val callLock = Any()
    private val readExecutor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "arb-ipc-read").apply { isDaemon = true }
    }

    val endpoint: String
        get() = transport.endpoint

    // ---------------- System ----------------

    fun systemHello(verifyProtocolVersion: Boolean = true): SystemHelloResponse {
        val resp = call("system.hello", JsonObject(), SystemHelloResponse::class.java)
        if (verifyProtocolVersion && resp.protocolVersion != IPC_PROTOCOL_VERSION) {
            throw IOException(
                "Incompatible arbd IPC protocol version ${resp.protocolVersion}; expected $IPC_PROTOCOL_VERSION",
            )
        }
        return resp
    }

    fun systemStatus(): SystemStatusResponse =
        call("system.status", JsonObject(), SystemStatusResponse::class.java)

    fun systemShutdown(): ShutdownResponse =
        call("system.shutdown", JsonObject(), ShutdownResponse::class.java)

    internal fun shutdownForHandover(): ShutdownResponse =
        call("system.shutdown_for_handover", JsonObject(), ShutdownResponse::class.java)

    // ---------------- Persisted Config (T-1) ----------------

    fun configGet(): ConfigResponse =
        call("config.get", JsonObject(), ConfigResponse::class.java)

    fun configUpdate(patch: ArbConfigPatch): ConfigResponse =
        call("config.update", ProtocolJson.toConfigPatchJson(patch), ConfigResponse::class.java)

    // ---------------- Device & Capture ----------------

    fun deviceList(): DeviceListResponse =
        call("device.list", JsonObject(), DeviceListResponse::class.java)

    fun captureStart(params: CaptureStartParams = CaptureStartParams()): CaptureStartResponse =
        call("capture.start", gson.toJsonTree(params), CaptureStartResponse::class.java)

    fun captureStop(): CaptureStopResponse =
        call("capture.stop", JsonObject(), CaptureStopResponse::class.java)

    // ---------------- Save Jobs ----------------

    fun saveTrigger(params: SaveTriggerParams = SaveTriggerParams()): SaveOutcomeResponse =
        call("save.trigger", gson.toJsonTree(params), SaveOutcomeResponse::class.java)

    fun saveLogs(params: SaveLogsParams = SaveLogsParams()): SaveOutcomeResponse =
        call("save.logs", gson.toJsonTree(params), SaveOutcomeResponse::class.java)

    fun saveScreenshot(params: SaveScreenshotParams = SaveScreenshotParams()): ScreenshotResult =
        call("save.screenshot", gson.toJsonTree(params), ScreenshotResult::class.java)

    fun saveListJobs(): SaveListJobsResponse =
        call("save.list_jobs", JsonObject(), SaveListJobsResponse::class.java)

    fun saveRetry(jobId: String): SaveOutcomeResponse =
        call("save.retry", jsonObj("job_id" to jobId), SaveOutcomeResponse::class.java)

    fun saveRetarget(jobId: String, destDir: String): SaveOutcomeResponse =
        call("save.retarget", jsonObj("job_id" to jobId, "dest_dir" to destDir), SaveOutcomeResponse::class.java)

    fun saveDiscard(jobId: String): SaveJobView =
        call("save.discard", jsonObj("job_id" to jobId), SaveJobView::class.java)

    // ---------------- Bundles & Leases ----------------

    fun bundleList(state: BundleLifecycleState? = null): BundleListResponse {
        val params = JsonObject()
        if (state != null) {
            params.addProperty("state", state.wire)
        }
        return call("bundle.list", params, BundleListResponse::class.java)
    }

    fun bundleOpen(
        bundleIdOrPath: String,
        holderKind: LeaseHolderKind = LeaseHolderKind.IDE_VIEWER,
        holderPid: Long = ProcessHandle.current().pid(),
    ): BundleOpenResponse {
        val params = JsonObject().apply {
            addProperty("bundle_id_or_path", bundleIdOrPath)
            addProperty("holder_kind", holderKind.wire)
            addProperty("holder_pid", holderPid)
        }
        return call("bundle.open", params, BundleOpenResponse::class.java)
    }

    fun bundleLeaseRenew(leaseId: String): LeaseRenewResponse =
        call("bundle.lease_renew", jsonObj("lease_id" to leaseId), LeaseRenewResponse::class.java)

    fun bundleClose(leaseId: String): LeaseCloseResponse =
        call("bundle.close", jsonObj("lease_id" to leaseId), LeaseCloseResponse::class.java)

    fun bundleTrash(bundleId: String): BundleEntryView =
        call("bundle.trash", jsonObj("bundle_id" to bundleId), BundleEntryView::class.java)

    fun bundleRestore(bundleId: String): BundleEntryView =
        call("bundle.restore", jsonObj("bundle_id" to bundleId), BundleEntryView::class.java)

    fun bundleGc(nowUtc: String? = null): BundleGcResponse {
        val params = JsonObject()
        if (nowUtc != null) {
            params.addProperty("now_utc", nowUtc)
        }
        return call("bundle.gc", params, BundleGcResponse::class.java)
    }

    // ---------------- Timeline ----------------

    fun timelineSummary(bundleIdOrPath: String): TimelineSummaryResponse =
        call("timeline.summary", jsonObj("bundle_id_or_path" to bundleIdOrPath), TimelineSummaryResponse::class.java)

    fun timelineQueryLogs(
        bundleIdOrPath: String,
        bucket: QueryBucket? = null,
        startNs: Long? = null,
        endNs: Long? = null,
        limit: Int? = null,
        packageName: String? = null,
        tag: String? = null,
        level: String? = null,
        keyword: String? = null,
        afterSeq: Long? = null,
    ): TimelineLogsResponse {
        val params = buildTimelineQueryParams(bundleIdOrPath, bucket, startNs, endNs, limit).apply {
            if (afterSeq != null) addProperty("after_seq", afterSeq)
            if (!packageName.isNullOrBlank()) addProperty("package", packageName)
            if (!tag.isNullOrBlank()) addProperty("tag", tag)
            if (!level.isNullOrBlank()) addProperty("level", level)
            if (!keyword.isNullOrBlank()) addProperty("keyword", keyword)
        }
        return call("timeline.query_logs", params, TimelineLogsResponse::class.java)
    }

    fun timelineQueryTaps(
        bundleIdOrPath: String,
        bucket: QueryBucket? = null,
        startNs: Long? = null,
        endNs: Long? = null,
        limit: Int? = null,
    ): TimelineTapsResponse {
        val params = buildTimelineQueryParams(bundleIdOrPath, bucket, startNs, endNs, limit)
        return call("timeline.query_taps", params, TimelineTapsResponse::class.java)
    }

    fun timelineDiscontinuities(bundleIdOrPath: String): TimelineDiscontinuitiesResponse =
        call(
            "timeline.discontinuities",
            jsonObj("bundle_id_or_path" to bundleIdOrPath),
            TimelineDiscontinuitiesResponse::class.java,
        )

    fun timelineCrashes(bundleIdOrPath: String): TimelineCrashesResponse =
        call(
            "timeline.crashes",
            jsonObj("bundle_id_or_path" to bundleIdOrPath),
            TimelineCrashesResponse::class.java,
        )

    fun timelineScreens(bundleIdOrPath: String): TimelineScreensResponse =
        call(
            "timeline.screens",
            jsonObj("bundle_id_or_path" to bundleIdOrPath),
            TimelineScreensResponse::class.java,
        )

    // ---------------- Edit Session ----------------

    fun editGetSession(sessionId: String): EditSessionView? =
        call("edit.get_session", jsonObj("session_id" to sessionId), EditSessionEnvelope::class.java).session

    fun editUpdateSession(params: EditUpdateSessionParams): EditSessionView =
        call("edit.update_session", gson.toJsonTree(params), EditSessionEnvelope::class.java).session
            ?: throw IOException("arbd edit.update_session returned null session")

    // ---------------- Export ----------------

    fun exportTrim(params: ExportTrimParams): ExportTrimResponse =
        call("export.trim", gson.toJsonTree(params), ExportTrimResponse::class.java, timeoutMs = longRunningReadTimeoutMs)

    fun exportPng(params: ExportPngParams): ExportPngResponse =
        call("export.png", gson.toJsonTree(params), ExportPngResponse::class.java)

    fun exportShareMp4(params: ExportShareMp4Params): ExportShareMp4Response =
        call("export.share_mp4", gson.toJsonTree(params), ExportShareMp4Response::class.java, timeoutMs = longRunningReadTimeoutMs)

    /**
     * Forward-compatible T-2B RPC (`export.share_mp4`). Returns `null` cleanly when connected
     * to a daemon build that has not implemented `export.share_mp4` yet (`-32601`), so callers
     * can fall back seamlessly to `export.trim`.
     */
    fun exportShareMp4OrNull(params: ExportShareMp4Params): ExportShareMp4Response? = try {
        exportShareMp4(params)
    } catch (e: ArbdRpcException) {
        if (e.isMethodNotFound) null else throw e
    }

    // ---------------- Media / SHM Viewport ----------------

    fun mediaOpenViewport(bundleIdOrPath: String): MediaOpenViewportResponse =
        call(
            "media.open_viewport",
            jsonObj("bundle_id_or_path" to bundleIdOrPath),
            MediaOpenViewportResponse::class.java,
        )

    fun mediaFilmstrip(
        viewportId: String,
        maxWidth: Int? = null,
        maxHeight: Int? = null,
    ): MediaFilmstripResponse {
        val params = JsonObject().apply {
            addProperty("viewport_id", viewportId)
            if (maxWidth != null) addProperty("max_width", maxWidth)
            if (maxHeight != null) addProperty("max_height", maxHeight)
        }
        return call("media.filmstrip", params, MediaFilmstripResponse::class.java)
    }

    fun mediaScrub(viewportId: String, frameIndex: Long): MediaScrubResponse {
        val params = JsonObject().apply {
            addProperty("viewport_id", viewportId)
            addProperty("frame_index", frameIndex)
        }
        return call("media.scrub", params, MediaScrubResponse::class.java)
    }

    fun mediaSeekFrame(viewportId: String, frameIndex: Long): MediaSeekFrameResponse {
        val params = JsonObject().apply {
            addProperty("viewport_id", viewportId)
            addProperty("frame_index", frameIndex)
        }
        return call("media.seek_frame", params, MediaSeekFrameResponse::class.java)
    }

    fun mediaCloseViewport(viewportId: String): MediaCloseViewportResponse =
        call("media.close_viewport", jsonObj("viewport_id" to viewportId), MediaCloseViewportResponse::class.java)

    fun mediaDecodeAudioPcm(
        bundleIdOrPath: String,
        outputPath: String? = null,
    ): MediaDecodeAudioPcmResponse = call(
        "media.decode_audio_pcm",
        gson.toJsonTree(MediaDecodeAudioPcmParams(bundleIdOrPath, outputPath)),
        MediaDecodeAudioPcmResponse::class.java,
        timeoutMs = longRunningReadTimeoutMs,
    )

    /**
     * Forward-compatible T-2B RPC (`media.decode_audio_pcm`). Returns `null` cleanly when the
     * daemon does not expose this RPC yet (`-32601`), allowing video-only playback without error.
     */
    fun mediaDecodeAudioPcmOrNull(
        bundleIdOrPath: String,
        outputPath: String? = null,
    ): MediaDecodeAudioPcmResponse? = try {
        mediaDecodeAudioPcm(bundleIdOrPath, outputPath)
    } catch (e: ArbdRpcException) {
        if (e.isMethodNotFound) null else throw e
    }

    // ---------------- Low-level JSON-RPC 2.0 ----------------

    fun <T> call(
        method: String,
        params: JsonElement,
        responseType: Class<T>,
        timeoutMs: Long = readTimeoutMs,
    ): T {
        val resultElement = callRaw(method, params, timeoutMs)
        return gson.fromJson(resultElement, responseType)
            ?: throw IOException("arbd returned null result for method '$method'")
    }

    fun callRaw(
        method: String,
        params: JsonElement = JsonObject(),
        timeoutMs: Long = readTimeoutMs,
    ): JsonElement = synchronized(callLock) {
        val id = nextId.getAndIncrement()
        val req = JsonObject().apply {
            addProperty("jsonrpc", JSONRPC_VERSION)
            addProperty("id", id)
            addProperty("method", method)
            add("params", params)
        }
        val rawLine = try {
            transport.writeLine(envelopeGson.toJson(req))
            readResponseLine(timeoutMs)
                ?: throw IOException("arbd closed IPC connection before responding to '$method'")
        } catch (e: IOException) {
            // Tag every transport-level failure with its method so the retry policy can
            // decide whether replaying it is safe (R4 S4).
            throw if (e is ArbdIoException) e else ArbdIoException(method, e)
        }
        val respObj = JsonParser.parseString(rawLine).asJsonObject
        // R4 C5: reject a response that does not echo this request's id — a desynced or
        // cross-talking peer must never satisfy the wrong request.
        val respId = respObj.get("id")
        if (respId == null || !respId.isJsonPrimitive || !respId.asJsonPrimitive.isNumber ||
            runCatching { respId.asLong }.getOrDefault(-1L) != id
        ) {
            throw IOException("arbd response id mismatch for '$method': expected $id, got $respId")
        }
        val errorElem = respObj.get("error")
        if (errorElem != null && !errorElem.isJsonNull) {
            val errObj = errorElem.asJsonObject
            val code = errObj.get("code")?.asInt ?: -32603
            val message = errObj.get("message")?.asString ?: "Unknown RPC error"
            throw ArbdRpcException(code = code, message = message, method = method)
        }
        return respObj.get("result") ?: JsonNull.INSTANCE
    }

    /**
     * Reads one NDJSON response line with a bounded wait (R4 S5/C5). On timeout the transport
     * is closed — a late response must never desync the framing of the next call.
     */
    private fun readResponseLine(timeoutMs: Long): String? = try {
        readExecutor.submit(Callable { transport.readLine() }).get(timeoutMs, TimeUnit.MILLISECONDS)
    } catch (e: TimeoutException) {
        runCatching { transport.close() }
        throw IOException("arbd IPC read timed out after ${timeoutMs} ms; transport closed")
    } catch (e: ExecutionException) {
        throw e.cause as? IOException ?: IOException("arbd IPC read failed", e.cause)
    } catch (e: InterruptedException) {
        Thread.currentThread().interrupt()
        throw IOException("arbd IPC read interrupted", e)
    }

    override fun close() {
        runCatching { transport.close() }
        readExecutor.shutdown()
    }

    private fun jsonObj(vararg pairs: Pair<String, String>): JsonObject = JsonObject().apply {
        for ((k, v) in pairs) {
            addProperty(k, v)
        }
    }

    private fun buildTimelineQueryParams(
        bundleIdOrPath: String,
        bucket: QueryBucket?,
        startNs: Long?,
        endNs: Long?,
        limit: Int?,
    ): JsonObject = JsonObject().apply {
        addProperty("bundle_id_or_path", bundleIdOrPath)
        if (bucket != null) addProperty("bucket", bucket.wire)
        if (startNs != null) addProperty("start_ns", startNs)
        if (endNs != null) addProperty("end_ns", endNs)
        if (limit != null) addProperty("limit", limit)
    }

    companion object {
        @Throws(IOException::class)
        fun connect(endpoint: String, os: HostOs = HostOs.detect()): ArbdRpcClient =
            ArbdRpcClient(LocalIpcTransport.connect(endpoint, os))
    }
}

package io.github.shinma06.arb.testsupport

import com.google.gson.JsonObject
import io.github.shinma06.arb.ide.ArbBundleFileEditor
import io.github.shinma06.arb.ide.ArbBundleVirtualFile
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.ide.SourceJumpResolver
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.playback.PlaybackClock
import io.github.shinma06.arb.state.AuxStream
import io.github.shinma06.arb.state.StreamLoadState
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

enum class Fault { NONE, BUSINESS_ERROR, TRANSPORT_DOWN }

class VirtualClock : PlaybackClock {
    private val now = AtomicLong(1_000_000_000L)
    override fun nowNs(): Long = now.get()
    fun advanceMs(ms: Long) {
        now.addAndGet(ms * 1_000_000L)
    }
}

/** A scripted daemon serving one video bundle with per-method fault injection and an edit-session store. */
class StudioDaemon(tmp: Path, val frameCount: Int = 10, val mainLogCount: Int = 0) {
    val faults = java.util.concurrent.ConcurrentHashMap<String, Fault>()
    @Volatile var loseEditResponse = false
    val storedSession = AtomicReference<String?>(null)
    val storedRanges = java.util.concurrent.CopyOnWriteArrayList<Triple<Long?, Long?, Long?>>()
    val crashes = AtomicReference(crashesJson())

    /** When set, `export.trim` parks on this latch (after counting) before answering. */
    @Volatile var exportGate: CountDownLatch? = null
    val generation = AtomicLong(1L)
    private val shmPath = writeValidShmFile(tmp.resolve("vp.shm"))
    private val shm = shmPath.toString().replace("\\", "\\\\")
    val transport: ScriptedTransport = ScriptedTransport { method, params -> handle(method, params) }

    private fun handle(method: String, params: JsonObject): String {
        when (faults[method] ?: Fault.NONE) {
            Fault.BUSINESS_ERROR -> return rpcError("boom-$method")
            Fault.TRANSPORT_DOWN -> throw TransportDown("reset-$method")
            Fault.NONE -> Unit
        }
        return when (method) {
            "bundle.open" -> bundleOpenResultJson("bundle", frameCount, 10_000_000_000L, 10_000_000_000L + frameCount * 33_333_333L)
            "edit.get_session" -> """{"session":${storedSession.get() ?: "null"}}"""
            "edit.update_session" -> {
                fun p(name: String) = params.get(name)?.takeIf { !it.isJsonNull }?.asLong
                storedRanges += Triple(p("in_frame_index"), p("out_frame_index"), p("playhead_frame_index"))
                val json = """{"session_id":"ide-bundle","bundle_id":"bundle","in_frame_index":${p("in_frame_index")},""" +
                    """"out_frame_index":${p("out_frame_index")},"playhead_frame_index":${p("playhead_frame_index")},""" +
                    """"updated_at_utc":"2026-11-25T00:00:00Z"}"""
                storedSession.set(json)
                if (loseEditResponse) throw TransportDown("response lost")
                """{"session":$json}"""
            }
            "export.trim" -> {
                exportGate?.await(10, java.util.concurrent.TimeUnit.SECONDS)
                """{"bundle_id":"trimmed","bundle_path":"/tmp/trimmed","trim_report":{"in_frame_index":0,"out_frame_index":9,"output_frame_count":10,"head_decoded_frames":0,"reencoded_frames":0,"stream_copied_frames":10,"backend_name":"x","edts_box_count":0,"elst_box_count":0},"unanchored_summary":{"included_count":0,"truncated_count":0},"main_log_count":0,"unanchored_log_count":0,"main_tap_count":0,"unanchored_tap_count":0}"""
            }
            "media.open_viewport" ->
                """{"viewport_id":"vp","bundle_id":"bundle","shm_path":"$shm","width":2,"height":2,"stride":8,"pixel_format":"rgba8","slot_capacity":64,"frame_count":$frameCount,"keyframe_count":2}"""
            "media.seek_frame" -> {
                val idx = params.get("frame_index").asLong
                val gen = generation.incrementAndGet()
                writeValidShmFile(shmPath, idx, gen)
                """{"generation":$gen,"frame_index":$idx,"is_approximate":false,"width":2,"height":2,"cache_hits":1,"cache_misses":0,"elapsed_ms":1}"""
            }
            "media.scrub" -> {
                val idx = params.get("frame_index").asLong
                val gen = generation.incrementAndGet()
                writeValidShmFile(shmPath, idx, gen)
                """{"generation":$gen,"requested_frame_index":$idx,"displayed_frame_index":$idx,"is_approximate":false,"width":2,"height":2,"cache_hits":1,"cache_misses":0,"elapsed_ms":1}"""
            }
            "media.filmstrip" -> """{"origin":"cache","cache_path":"/tmp/f","count":0,"elapsed_ms":1,"entries":[]}"""
            "timeline.discontinuities" -> """{"discontinuities":[]}"""
            "timeline.crashes" -> crashes.get()
            "timeline.screens" -> """{"screens":[]}"""
            "timeline.query_taps" -> """{"taps":[]}"""
            "timeline.query_logs" -> {
                val main = params.get("bucket")?.asString == "main"
                val after = params.get("after_seq")?.takeIf { !it.isJsonNull }?.asLong ?: 0L
                val limit = params.get("limit")?.asInt ?: 10_000
                val rows = if (main) ((after + 1)..mainLogCount.toLong()).take(limit) else emptyList()
                """{"logs":[${rows.joinToString(",") { logRowJson(it) }}]}"""
            }
            "media.close_viewport" -> """{"closed":true,"viewport_id":"vp"}"""
            "bundle.close" -> """{"released":true,"lease_id":"lease-bundle"}"""
            else -> "{}"
        }
    }
}

private val noopJump = object : SourceJumpResolver {
    override fun resolveAndNavigate(target: io.github.shinma06.arb.state.SourceJumpTarget): Boolean = false
}

/** Opens a Studio editor against [daemon]; [publishEdt] defaults to running inline (deterministic). */
fun openStudioEditor(
    daemon: StudioDaemon,
    clock: PlaybackClock = VirtualClock(),
    publishEdt: (() -> Unit) -> Unit = { it() },
): ArbBundleFileEditor {
    val rpc = daemon.transport.newConnection()
    val service = object : ArbdDaemonService() {
        override fun openMediaClient() = daemon.transport.newConnection()
        override fun <T> withClient(block: (ArbdRpcClient) -> T): T = block(rpc)
    }
    return ArbBundleFileEditor(
        project = null,
        backingFile = ArbBundleVirtualFile("bundle", "bundle"),
        bundleIdOrPath = "bundle",
        displayTitle = "bundle",
        daemonService = service,
        sourceJumpResolver = noopJump,
        playbackClock = clock,
        autoRunPlaybackLoop = false,
        audioAutoRunPump = false,
        publishEdt = publishEdt,
    )
}

fun ArbBundleFileEditor.awaitReady() {
    awaitTrue({ "editor never became ready: ${studioController.errorMessage}" }) {
        studioController.viewerState != null && playbackControllerForTest() != null
    }
}

fun ArbBundleFileEditor.awaitAuxSettled() {
    awaitTrue({ "aux streams never settled: ${studioController.viewerState?.auxStreams}" }) {
        val s = studioController.viewerState ?: return@awaitTrue false
        AuxStream.entries.all { s.streamState(it) !is StreamLoadState.Pending }
    }
}

package io.github.shinma06.arb.ide

import com.google.gson.Gson
import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.playback.FrameSeekResult
import io.github.shinma06.arb.playback.FrameSeeker
import io.github.shinma06.arb.playback.PlaybackClock
import io.github.shinma06.arb.playback.PlaybackController
import io.github.shinma06.arb.testsupport.ScriptedTransport
import io.github.shinma06.arb.testsupport.rpcError
import io.github.shinma06.arb.testsupport.writeValidShmFile
import io.github.shinma06.arb.ui.ArbStudioController
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.TestFactory
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlin.test.assertEquals

class PddS6PlaybackTraceReplayTest {
    @TempDir
    lateinit var tmp: Path

    @TestFactory
    fun replay(): List<DynamicTest> {
        val root = Path.of(System.getProperty("arb.repo.root"))
        val file = root.resolve("docs/specs/playback-s6-traces.json")
        return JsonParser.parseString(Files.readString(file)).asJsonArray.map { element ->
            val trace = element.asJsonObject
            DynamicTest.dynamicTest(trace["name"].asString) {
                Replay(tmp, trace["selection"].asString).use { replay ->
                    trace["events"].asJsonArray.forEach(replay::apply)
                    assertEquals(trace["expected"], replay.observe())
                }
            }
        }
    }

    // A synchronous controller method may be blocked in a seek while a second caller
    // changes its generation. Wait for its lock-protected publication, RPC, or return.
    private class Call {
        val started = CountDownLatch(1)
        val done = CompletableFuture<Unit>()
    }

    private class Gate(val method: String, val frame: Long, val call: Call) {
        val response = CompletableFuture<String>()
        var displayed = 0L
    }

    private class Replay(tmp: Path, selection: String) : AutoCloseable {
        private val executor = Executors.newCachedThreadPool()
        private val active = ThreadLocal<Call>()
        private val calls = mutableListOf<Call>()
        private val gates = LinkedBlockingQueue<Gate>()
        private var gate: Gate? = null
        private var replaying = false
        private val sockets = CopyOnWriteArrayList<ScriptedTransport>()
        private val leases = mutableMapOf<String, ScriptedTransport>()
        private val disposed = AtomicBoolean()
        private val studio = ArbStudioController(onScrub = {}, onSeekExact = {}, onTrimExport = { _, _ -> }, onPngExport = {})
        private val seekCount = AtomicInteger()
        private val now = AtomicLong()
        private val shm = writeValidShmFile(Files.createTempFile(tmp, "s6", ".shm"))
        private val sharedHellos = AtomicInteger()
        private val service = ArbdDaemonService(
            processManager = ArbdProcessManager(ArbPaths(tmp.resolve("data"), tmp.resolve("cache"), tmp.resolve("run"))),
            connector = {
                val shared = ScriptedTransport { method, _ ->
                    assertEquals("system.hello", method)
                    if (sharedHellos.incrementAndGet() == 1) throw IOException("shared connection reset")
                    """{"protocol_version":1,"realtime_trim":true,"schema_version":2,"pid":1,"daemon_version":"test"}"""
                }
                val hello = SystemHelloResponse(1, 2, 1, "", "", "", "test")
                ArbdProcessManager.ConnectedDaemon(ArbdRpcClient(shared), hello)
            },
            connectMedia = {
                if (sockets.isNotEmpty() && exchange("connect", -1).first == "SharedRpcDisconnect") {
                    throw IOException("media endpoint unavailable")
                }
                connect()
            },
        )
        private val session = StudioViewportSession(service::openMediaClient, disposed::get)

        private val seeker = object : FrameSeeker {
            override fun seekExact(frameIndex: Long) = seekExact(frameIndex) {}
            override fun scrubApprox(frameIndex: Long): FrameSeekResult = error("unexpected scrub")
            override fun seekExact(frameIndex: Long, onRecovery: () -> Unit): FrameSeekResult {
                return session.seekFrame(frameIndex, onRecovery) {}
            }
        }
        private val controller = PlaybackController(
            totalFrames = 2, frameSeeker = seeker, framePtsNs = listOf(0, 3), lastFrameDurationNs = 1,
            initialLoopInSelection = selection != "Full", initialOutFrameIndex = if (selection == "SingleLoop") 0 else 1,
            clock = PlaybackClock(now::get), autoRunLoop = false, onPlaybackError = { studio.showPlaybackError(checkNotNull(it.message)) },
        )

        init {
            session.open("bundle")
            controller.addStateListener {
                studio.renderPlaybackState(controller.state)
                active.get()?.started?.countDown()
            }
        }

        private fun connect(): ArbdRpcClient {
            val index = sockets.size
            val id = "vp-$index"
            lateinit var socket: ScriptedTransport
            socket = ScriptedTransport { method, params ->
                when (method) {
                    "media.open_viewport" -> {
                        leases[id] = socket
                        if (index > 0 && exchange("open", -1).first == "ReopenFailed") {
                            throw IOException("viewport open reply lost")
                        }
                        val path = Gson().toJson(shm.toString())
                        """{"viewport_id":"$id","bundle_id":"bundle","shm_path":$path,"width":2,"height":2,"stride":8,"pixel_format":"rgba8","slot_capacity":64,"frame_count":2,"keyframe_count":1}"""
                    }
                    "media.seek_frame" -> {
                        assertEquals(id, params["viewport_id"].asString)
                        val frame = params["frame_index"].asLong
                        val generation = seekCount.incrementAndGet()
                        val (reply, displayed) = exchange("seek", frame)
                        when (reply) {
                            "ViewportNotFoundDisconnect" -> throw ArbdRpcException(-32001, "viewport expired")
                            "SeekPermanentError" -> rpcError("decode failed")
                            else -> {
                                val published = if (reply == "SeekTransientError") 0L else generation.toLong()
                                writeValidShmFile(shm, displayed, published)
                                """{"generation":$generation,"frame_index":$frame,"is_approximate":false,"width":2,"height":2,"cache_hits":0,"cache_misses":1,"elapsed_ms":0}"""
                            }
                        }
                    }
                    "media.close_viewport" -> {
                        assertEquals(id, params["viewport_id"].asString)
                        leases.remove(id)
                        """{"closed":true,"viewport_id":"$id"}"""
                    }
                    else -> error(method)
                }
            }
            sockets += socket
            return ArbdRpcClient(socket)
        }

        private fun exchange(method: String, frame: Long): Pair<String, Long> {
            val call = checkNotNull(active.get())
            val current = Gate(method, frame, call)
            gates.put(current)
            call.started.countDown()
            return current.response.get(5, TimeUnit.SECONDS) to current.displayed
        }

        private fun command(operation: () -> Unit) {
            val call = Call()
            calls += call
            executor.execute {
                active.set(call)
                try {
                    operation()
                    call.done.complete(Unit)
                } catch (e: Throwable) {
                    call.done.completeExceptionally(e)
                } finally {
                    call.started.countDown()
                    active.remove()
                }
            }
            check(call.started.await(5, TimeUnit.SECONDS))
        }

        private fun receive(method: String): Gate =
            checkNotNull(gates.poll(5, TimeUnit.SECONDS)) { "missing $method" }.also { assertEquals(method, it.method) }

        fun apply(event: JsonElement) {
            val name = if (event.isJsonPrimitive) event.asString else event.asJsonObject.keySet().single()
            val arg = if (event.isJsonObject) event.asJsonObject[name].asLong else 0L
            when (name) {
                "Play" -> command(controller::play)
                "Pause" -> controller.pause()
                "Elapse" -> now.addAndGet(arg)
                "AdvanceClock" -> command {
                    now.addAndGet(arg)
                    controller.tick()
                }
                "UserSeekTime" -> command { controller.seekToTime(arg) }
                "StepFrame" -> command { controller.stepFrame(arg.toInt()) }
                "SeekRequestIssued" -> {
                    gate = receive("seek").also { assertEquals(arg, it.frame) }
                    replaying = false
                }
                "ViewportNotFoundDisconnect" -> {
                    val current = checkNotNull(gate)
                    current.response.complete(name)
                    gate = if (replaying) {
                        current.call.done.get(5, TimeUnit.SECONDS)
                        null
                    } else {
                        receive("connect")
                    }
                }
                "AcquireMissing" -> gate = receive("connect")
                "ReopenFailed" -> {
                    val opening = checkNotNull(gate)
                    opening.response.complete(name)
                    opening.call.done.get(5, TimeUnit.SECONDS)
                    gate = null
                }
                "SharedRpcDisconnect" -> {
                    checkNotNull(gate).response.complete(name)
                    gate = receive("connect")
                    assertEquals(2, sharedHellos.get())
                }
                "ConnectViewport" -> {
                    checkNotNull(gate).response.complete(name)
                    gate = receive("open")
                }
                "Reopen" -> {
                    val opening = checkNotNull(gate)
                    opening.response.complete(name)
                    gate = if (disposed.get()) {
                        opening.call.done.get(5, TimeUnit.SECONDS)
                        null
                    } else {
                        receive("seek")
                    }
                    replaying = true
                }
                "SeekSuccess", "SeekTransientError", "SeekPermanentError" -> {
                    val current = checkNotNull(gate)
                    current.displayed = arg
                    current.response.complete(name)
                    if (name != "SeekTransientError") current.call.done.get(5, TimeUnit.SECONDS)
                    gate = null
                }
                "Dispose" -> {
                    disposed.set(true)
                    controller.close()
                }
                "CloseSession" -> session.close()
                else -> error(name)
            }
        }

        fun observe(): JsonObject {
            calls.forEach { it.done.get(5, TimeUnit.SECONDS) }
            check(gates.isEmpty()) { "unexpected RPC after the last trace event" }
            return JsonObject().apply {
                addProperty("mode", controller.state.mode.name)
                addProperty("playheadTimeNs", controller.displayPositionNs())
                addProperty("displayedFrame", controller.state.displayedFrameIndex)
                addProperty("errorVisible", !studio.playbackError.isNullOrBlank())
                addProperty("seekCount", seekCount.get())
                close()
                // arbd closes the leases of a socket on EOF.
                addProperty("openViewportsAfterDispose", leases.values.count { !it.closed.get() })
                check(sockets.all { it.closed.get() })
            }
        }

        override fun close() {
            disposed.set(true)
            executor.shutdownNow()
            controller.close()
            session.close()
            service.dispose()
        }
    }
}

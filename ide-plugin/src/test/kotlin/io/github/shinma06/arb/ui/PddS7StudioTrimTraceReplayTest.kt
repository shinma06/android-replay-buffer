package io.github.shinma06.arb.ui

import androidx.compose.ui.graphics.ImageBitmap
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import io.github.shinma06.arb.playback.PlaybackMode
import io.github.shinma06.arb.playback.PlaybackState
import io.github.shinma06.arb.playback.TimelineCoordinateAxis
import io.github.shinma06.arb.shm.ShmFrameMeta
import io.github.shinma06.arb.shm.ShmPixelFormat
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.TestFactory
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.assertEquals
import kotlin.test.assertNull

class PddS7StudioTrimTraceReplayTest {
    @TestFactory
    fun replay(): List<DynamicTest> {
        val file = Path.of(System.getProperty("arb.repo.root"), "docs/specs/studio-trim-s7-traces.json")
        val fixture = JsonParser.parseString(Files.readString(file)).asJsonObject
        return fixture["traces"].asJsonArray.map { element ->
            val trace = element.asJsonObject
            DynamicTest.dynamicTest(trace["name"].asString) {
                val replay = Replay(fixture)
                trace["steps"].asJsonArray.forEach { replay.step(it.asJsonObject) }
            }
        }
    }

    private class Replay(fixture: JsonObject) {
        private val starts = fixture["frameStartsNs"].asJsonArray.map { it.asLong }
        private val end = fixture["endNs"].asLong
        private val axis = TimelineCoordinateAxis(starts.size.toLong(), starts, lastDurationNs = end - starts.last())
        private val left = fixture["leftPx"].asFloat
        private val width = fixture["widthPx"].asFloat
        private var zoom = 1f
        private var scroll = 0L
        private var now = 0L
        private var pending: ((ImageBitmap?) -> Unit)? = null
        private var flight: ((ImageBitmap?) -> Unit)? = null

        // Only bitmap identity is observed. This exercises EDT publication on headless runners.
        private fun imageToken() = Proxy.newProxyInstance(ImageBitmap::class.java.classLoader, arrayOf(ImageBitmap::class.java)) {
                receiver, method, args ->
            when (method.name) {
                "equals" -> receiver === args?.first()
                "hashCode" -> System.identityHashCode(receiver)
                else -> error("unexpected image access ${method.name}")
            }
        } as ImageBitmap

        private val previewImage = imageToken()
        private var playbackImage = imageToken()

        private val studio = ArbStudioController(
            onScrub = { error("unexpected frame scrub") },
            onSeekExact = { error("unexpected frame seek") },
            onPngExport = { _, _ -> },
            onTrimExport = { _, _ -> },
            onTimeSeek = { time, _ -> now = time },
            displayPositionNs = { now },
            onTrimPreviewFrame = { _, deliver -> pending = deliver },
        )

        init {
            studio.renderFrame(studioVideoState().copy(axis = axis, frameCount = axis.count, outTimeNs = end), playbackImage, 1, 1)
        }

        fun step(row: JsonObject) {
            val event = row["action"]
            val name = if (event.isJsonObject) event.asJsonObject.keySet().single() else event.asString
            val arg = if (event.isJsonObject) event.asJsonObject[name] else null
            fun time() = checkNotNull(arg).asLong
            when (name) {
                "DragPlayheadStart", "DragPlayheadMove" -> studio.seekTime(time(), false)
                "DragPlayheadEnd" -> studio.seekTime(studio.playheadTimeNs)
                "DragInStart", "DragInMove" -> studio.dragTrim(TrimEdge.IN, time(), false)
                "DragOutStart", "DragOutMove" -> studio.dragTrim(TrimEdge.OUT, time(), false)
                "DragInEnd", "DragOutEnd" -> checkNotNull(studio.trimPreview).let { studio.dragTrim(it.edge, it.timeNs, true) }
                "DragInCancel", "DragOutCancel" -> studio.cancelGesture()
                "MarkInFromPlayhead" -> studio.markInAtPlayhead()
                "MarkOutFromPlayhead" -> studio.markOutAtPlayhead()
                "NudgeIn", "FrameIn" -> studio.nudge(TrimEdge.IN, time().toInt(), byTime = name == "NudgeIn")
                "NudgeOut", "FrameOut" -> studio.nudge(TrimEdge.OUT, time().toInt(), byTime = name == "NudgeOut")
                "TogglePlay" -> studio.renderPlaybackState(PlaybackState(
                    mode = if (studio.isPlaying) PlaybackMode.PAUSED else PlaybackMode.PLAYING,
                    currentFrameIndex = axis.frameAt(now), totalFrames = axis.count,
                ))
                "PlayTick" -> {
                    now = time()
                    studio.renderPlaybackState(studio.playbackState.copy(currentFrameIndex = axis.frameAt(now)))
                    studio.sampleClock()
                }
                "DecodeStart" -> {
                    assertNull(flight)
                    flight = checkNotNull(pending)
                    pending = null
                }
                "LateFrameArrives" -> {
                    playbackImage = imageToken()
                    val meta = ShmFrameMeta(1, axis.frameAt(now), false, 1, 1, 4, ShmPixelFormat.RGBA8, now)
                    studio.applyFramePublished(meta, playbackImage, 1, 1)
                    checkNotNull(flight)(previewImage)
                    flight = null
                }
                "ZoomChange" -> {
                    zoom = checkNotNull(arg).asJsonArray[0].asFloat
                    scroll = arg.asJsonArray[1].asLong
                }
                else -> error("unknown S7 action $name")
            }
            val state = checkNotNull(studio.viewerState)
            val geometry = StudioTimelineGeometry(state, left, width, zoom, scroll)
            assertEquals(row["playheadNs"].asLong, studio.playheadTimeNs, name)
            assertEquals(row["inNs"].asLong, state.inTimeNs, name)
            assertEquals(row["outNs"].asLong, state.outTimeNs, name)
            assertEquals(row["previewNs"].takeUnless { it.isJsonNull }?.asLong, studio.trimPreview?.timeNs, name)
            assertEquals(row["inX"].asFloat, geometry.inX, name)
            assertEquals(row["outX"].asFloat, geometry.outX, name)
            assertEquals(row["playheadX"].asFloat, geometry.playhead(studio.playheadTimeNs), name)
            kotlin.test.assertSame(if (row["previewDisplayed"].asBoolean) previewImage else playbackImage, studio.frameBitmap, name)
        }
    }
}

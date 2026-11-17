package io.github.shinma06.replaybuffer.ui

import kotlin.test.Test
import kotlin.test.assertEquals

class ReplayPlaybackFramesTest {
    @Test
    fun queuedOldDeliveryCannotConsumeANewSessionsFrame() {
        val harness = Harness()
        val old = harness.start()
        old(0L to "old")
        harness.stop()
        val current = harness.start()
        current(100L to "current")

        harness.drain()

        assertEquals(listOf<Pair<Long, String?>>(100L to "current"), harness.shown)
    }

    @Test
    fun lateOldFrameCannotReplaceANewSessionsFrame() {
        val harness = Harness()
        val old = harness.start()
        harness.stop()
        val current = harness.start()
        current(100L to "current")
        old(0L to "late old conversion")

        harness.drain()

        assertEquals(listOf<Pair<Long, String?>>(100L to "current"), harness.shown)
    }

    @Test
    fun lateOldGapCannotBlankANewSessionsFrame() {
        val harness = Harness()
        val old = harness.start()
        harness.stop()
        val current = harness.start()
        current(100L to "current")
        old(0L to null)

        harness.drain()

        assertEquals(listOf<Pair<Long, String?>>(100L to "current"), harness.shown)
    }

    @Test
    fun activeSessionCoalescesFramesAndGapsAndCanPublishAgain() {
        val harness = Harness()
        val current = harness.start()
        current(0L to "first")
        current(10L to "second")
        current(20L to null)
        assertEquals(1, harness.queue.size)
        harness.drain()
        assertEquals(listOf<Pair<Long, String?>>(20L to null), harness.shown)

        current(30L to "resumed")
        harness.drain()
        assertEquals(listOf(20L to null, 30L to "resumed"), harness.shown)

        current(40L to "stopped")
        harness.stop()
        harness.drain()
        assertEquals(listOf(20L to null, 30L to "resumed"), harness.shown)
    }

    private class Harness {
        val queue = ArrayDeque<() -> Unit>()
        val shown = mutableListOf<Pair<Long, String?>>()
        private val frames = ReplayPlaybackFrames<Pair<Long, String?>> { queue.addLast(it) }
        private var active = 0

        fun start(): (Pair<Long, String?>) -> Unit {
            val session = ++active
            return frames.session { if (active == session) shown += it }
        }

        fun stop() { active++; frames.clear() }

        fun drain() { while (queue.isNotEmpty()) queue.removeFirst()() }
    }
}

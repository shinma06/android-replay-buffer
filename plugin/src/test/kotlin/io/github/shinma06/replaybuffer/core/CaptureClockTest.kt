package io.github.shinma06.replaybuffer.core

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class CaptureClockTest {
    private fun receiptClock(roundTrip: Long, measurementDelay: Long = 100_000): CaptureClock = CaptureClock().apply {
        fun sample(elapsed: Long, sent: Long, received: Long) = add("1",
            listOf(elapsed, elapsed, 1_700_000_000_000_000_000 + elapsed, elapsed), sent, received)
        assertTrue(sample(1_000_000_000, 10_000_000_000 + measurementDelay - 100_000,
            10_000_000_000 + measurementDelay + 100_000))
        assertTrue(sample(2_000_000_000, 11_000_000_000, 11_000_000_000 + roundTrip))
    }

    @Test
    fun validRoundTripSupportsCausallyArrivedVideoAndLogWithEitherDelayDirection() {
        for (roundTrip in listOf(31_842_708L, 40_000_000L)) {
            for (measurementDelay in listOf(100_000L, roundTrip - 100_000)) {
                // Constant host-minus-device offset: measurement and transport delays are strictly positive.
                val clock = receiptClock(roundTrip, measurementDelay)
                val sample = clock.snapshot().last()
                val measurementHost = sample.sent + measurementDelay
                val eventHost = measurementHost + 34_000_000
                val host = maxOf(eventHost + 1_000_000, sample.received + 1_000_000)
                assertTrue(measurementHost > sample.sent && measurementHost < sample.received)
                assertTrue(host >= sample.received)
                assertTrue(eventHost < host)
                assertTrue(clock.certain(host))
                val video = clock.video(2_034_000, host)
                val log = clock.log(1_700_000_002_034_000_000, host)
                assertEquals(2_034_000_000, video.elapsed)
                assertEquals(video, log)
                assertEquals(1_034_000_000, log.sequence)
                assertEquals(1_000_000, log.uncertainty)
                assertEquals(0, clock.currentEpoch())
            }
        }
    }

    @Test
    fun receiptSupportKeepsItsExactSourceBoundaryAndFiveSecondStaleLimit() {
        val roundTrip = 31_842_708L
        val clock = receiptClock(roundTrip)
        val received = 11_000_000_000 + roundTrip
        val host = received + 3_000_000
        val upper = 2_000_000_000 + host - 11_000_000_000 + 20_000_000
        assertEquals(upper, clock.log(1_700_000_000_000_000_000 + upper, host).elapsed)
        assertNull(clock.log(1_700_000_000_000_000_000 + upper + 1, host).elapsed)
        assertTrue(clock.video(upper / 1000, host).elapsed != null)
        assertNull(clock.video(upper / 1000 + 1, host).elapsed)
        val lastFreshHost = received + 5_000_000_000
        val elapsed = 2_000_000_000 + 5_000_000_000 + roundTrip
        assertEquals(elapsed, clock.log(1_700_000_000_000_000_000 + elapsed, lastFreshHost).elapsed)
        assertTrue(clock.video(elapsed / 1000, lastFreshHost).elapsed != null)
        assertNull(clock.log(1_700_000_000_000_000_000 + elapsed, lastFreshHost + 1).elapsed)
        assertNull(clock.video(elapsed / 1000, lastFreshHost + 1).elapsed)
        assertNull(clock.log(1_699_999_999_000_000_000, host).elapsed)
        assertNull(clock.video(2_034_000, host, 99).elapsed)
    }

    @Test
    fun slowRoundTripStillMapsButOnlyWidensTheHostEstimate() {
        val clock = receiptClock(1_000_000)
        val roundTrip = 69_000_000L
        assertTrue(clock.add("1", listOf(3_000_000_000L, 3_000_000_000L, 1_700_000_003_000_000_000, 3_000_000_000),
            12_000_000_000, 12_000_000_000 + roundTrip))
        val slow = clock.snapshot().last()
        assertTrue(slow.valid)
        assertTrue(!slow.hostAnchored)
        val host = 12_100_000_000L
        assertTrue(clock.certain(host))
        assertEquals(0, clock.currentEpoch())
        assertEquals(3_034_000_000, clock.log(1_700_000_003_034_000_000, host).elapsed)
        assertEquals(3_034_000_000, clock.video(3_034_000, host).elapsed)
        // T and its error come from the latest round-trip-supported sample, not from the slow reply.
        val anchor = assertNotNull(clock.hostAnchor(host))
        assertEquals(11_001_000_000, anchor.received)
        assertEquals(1_000_000L + 500_000, clock.endUncertainty(host))
        assertEquals(2_000_000_000 + host - anchor.received - 1_000_000_000, clock.now(host))

        val onlySlow = CaptureClock()
        assertTrue(onlySlow.add("1", listOf(2_000_000_000L, 2_000_000_000L,
            1_700_000_002_000_000_000, 2_000_000_000), 11_000_000_000, 11_000_000_000 + roundTrip))
        assertEquals(1_000_000L + roundTrip / 2, onlySlow.endUncertainty(11_100_000_000))
        assertEquals(2_034_000_000, onlySlow.log(1_700_000_002_034_000_000, 11_100_000_000).elapsed)
    }

    @Test
    fun hostAnchorOlderThanFiveSecondsAtTheCallerTimeIsNotUsed() {
        val clock = receiptClock(1_000_000) // Round-trip-supported sample received at 11.001 s.
        val roundTrip = 69_000_000L
        assertTrue(clock.add("1", listOf(6_000_000_000L, 6_000_000_000L, 1_700_000_006_000_000_000, 6_000_000_000),
            15_000_000_000, 15_000_000_000 + roundTrip))
        val fresh = 15_100_000_000L
        assertEquals(11_001_000_000, assertNotNull(clock.hostAnchor(fresh)).received)
        assertEquals(1_000_000L + 500_000, clock.endUncertainty(fresh))
        // At 19 s the supported sample is ~8 s old: T and its error come from the slow but fresh sample.
        val late = 19_000_000_000L
        val slow = clock.snapshot().last()
        assertEquals(slow, clock.hostAnchor(late))
        assertEquals(slow.readError + roundTrip / 2, clock.endUncertainty(late))
        assertEquals(slow.elapsed + slow.sequenceOffset + late - slow.received, clock.now(late))
    }

    @Test
    fun receiptSupportDoesNotValidateInvalidSamplesOrCrossClosedEpochs() {
        val invalid = CaptureClock()
        assertEquals(false, invalid.add("1", listOf(2_000_000_000L, 2_000_000_000L,
            1_700_000_002_000_000_000, 2_002_000_001), 11_000_000_000, 11_001_000_000))
        assertNull(invalid.log(1_700_000_002_034_000_000, 11_050_000_000).elapsed)
        assertNull(invalid.video(2_034_000, 11_050_000_000).elapsed)

        val clock = receiptClock(31_842_708)
        val closedAt = 11_036_842_708L
        clock.boundary(closedAt)
        assertTrue(clock.add("1", listOf(3_000_000_000L, 3_000_000_000L,
            1_700_000_003_000_000_000, 3_000_000_000), 12_000_000_000, 12_000_200_000))
        assertNull(clock.log(1_700_000_002_000_000_000, closedAt + 1).elapsed)
        assertNull(clock.log(1_700_000_002_034_000_000, 11_035_100_000).elapsed)
        assertNull(clock.video(2_034_000, 11_035_100_000, 0).elapsed)
        assertEquals(3_000_000_000, clock.log(1_700_000_003_000_000_000, 12_000_200_000).elapsed)
    }

    private fun add(clock: CaptureClock, elapsed: Long, mono: Long = elapsed, wall: Long = 1_700_000_000_000_000_000 + elapsed,
                    host: Long = elapsed, boot: String = "1") {
        clock.add(boot, listOf(elapsed, mono, wall, elapsed + 100_000), host, host + 200_000)
    }

    @Test
    fun continuousReadAfterAnOutageResolvesTheUnobservedIntervalInTheSameEpoch() {
        val clock = CaptureClock()
        add(clock, 1_000_000_000)
        val uncertain = clock.video(7_000_000, 7_000_000_000)
        assertNull(uncertain.elapsed) // Stale while the outage lasts.
        add(clock, 11_000_000_000)
        assertEquals(listOf(0), clock.snapshot().map { it.epoch }.distinct())
        assertEquals(7_000_050_000, clock.video(7_000_000, 7_000_000_000, uncertain.epoch).elapsed)
        assertEquals(7_000_050_000, clock.log(1_700_000_007_000_000_000, 7_000_000_000).elapsed)
        assertTrue(clock.video(11_000_000, 11_000_200_000).elapsed != null)
    }

    @Test
    fun discontinuousReadAfterAnOutageStillStartsANewEpoch() {
        for (kind in listOf("sleep", "wall", "boot")) {
            val clock = CaptureClock()
            add(clock, 1_000_000_000)
            when (kind) {
                "sleep" -> add(clock, 11_000_000_000, mono = 8_000_000_000) // The device slept: E-mono changed.
                "wall" -> add(clock, 11_000_000_000, wall = 1_700_000_000_000_000_000 + 60_000_000_000)
                else -> add(clock, 11_000_000_000, boot = "2")
            }
            assertEquals(1, clock.currentEpoch(), kind)
            assertNull(clock.video(7_000_000, 7_000_000_000, 0).elapsed, kind)
            assertNull(clock.log(1_700_000_007_000_000_000, 7_000_000_000).elapsed, kind)
        }
    }

    @Test
    fun slowRoundTripAdvancesEstimatedWindowAndKeepsSourceSynchronization() {
        val clock = CaptureClock()
        assertTrue(clock.add("1", listOf(1_000_000_000L, 1_000_000_000L, 1_700_000_001_000_000_000L, 1_000_000_000L),
            1_000_000_000, 1_100_000_000))
        assertTrue(clock.add("1", listOf(2_000_000_000L, 2_000_000_000L, 1_700_000_002_000_000_000L, 2_000_000_000L),
            2_000_000_000, 2_100_000_000))
        assertTrue(clock.now(2_200_000_000)!! >= 1_000_000_000)
        assertEquals(2_000_000_000, clock.video(2_000_000, 2_200_000_000).elapsed)
        assertEquals(1_000_000L + 50_000_000, clock.endUncertainty(2_200_000_000))
    }

    @Test
    fun videoAndLogUseTheSameElapsedWithInterpolationAndIgnoreTinyOffsetChanges() {
        val clock = CaptureClock()
        add(clock, 1_000_000_000)
        add(clock, 2_000_000_000, mono = 1_999_800_000)
        assertEquals(1, clock.snapshot().map { it.epoch }.distinct().size)
        val video = clock.video(1_500_000, 2_000_200_000)
        val log = clock.log(1_700_000_001_500_000_000, 2_000_200_000)
        assertTrue(kotlin.math.abs(video.elapsed!! - 1_500_150_000) <= 100)
        assertEquals(1_500_050_000, log.elapsed)
        assertTrue(video.uncertainty < 20_000_000)
        assertNull(clock.video(10_000_000, 10_000_000_000).elapsed)
    }

    @Test
    fun backwardWallJumpMakesOverlappingEpochRecordsAmbiguousAndBootKeepsSequence() {
        val clock = CaptureClock()
        add(clock, 1_000_000_000)
        add(clock, 2_000_000_000)
        add(clock, 3_000_000_000, wall = 1_700_000_001_000_000_000)
        add(clock, 4_000_000_000, wall = 1_700_000_002_000_000_000)
        assertNull(clock.log(1_700_000_001_500_000_000, 4_000_200_000).elapsed)
        add(clock, 500_000_000, host = 5_000_000_000, boot = "2")
        val afterBoot = clock.video(500_000, 5_000_200_000)
        assertEquals(500_050_000, afterBoot.elapsed)
        assertTrue(afterBoot.sequence!! > 3_000_000_000)
        // A host-clock jump no longer matters: each boot has its own exact BOOTTIME segment.
        add(clock, 200_000_000, host = 6_000_000_000, boot = "3")
        val third = clock.video(200_000, 6_000_200_000)
        assertEquals(200_050_000, third.elapsed)
        assertTrue(third.sequence!! > afterBoot.sequence!!)
        assertTrue(third.uncertainty < 20_000_000)
        // Boot 2's rows keep their BOOTTIME but get no time on boot 3's axis.
        val previousBoot = clock.video(500_000, 6_000_200_000, afterBoot.epoch)
        assertEquals(500_050_000, previousBoot.elapsed)
        assertNull(previousBoot.sequence)
        assertEquals(Long.MAX_VALUE, previousBoot.uncertainty)
        assertEquals(afterBoot, clock.video(500_000, 6_000_200_000, afterBoot.epoch, clock.segment(afterBoot.epoch)))
        assertEquals(6, clock.snapshot().size)
    }
    @Test
    fun fixedLogMapperMatchesLinearMappingAcrossBoundariesAndSurvivesClockMutation() {
        val clock = CaptureClock()
        val wallBase = 1_700_000_000_000_000_000L
        fun compare() {
            val mapper = clock.logMapper()
            val snapshot = clock.snapshot()
            val walls = snapshot.flatMap { sample ->
                listOf(-20_000_001L, -20_000_000L, -1L, 0L, 1L, 20_000_000L, 20_000_001L,
                    5_000_000_000L, 5_000_000_001L).map { sample.wall + it }
            } + listOf(0L, Long.MIN_VALUE, Long.MAX_VALUE, wallBase + 1_500_000_000)
            val hosts = snapshot.flatMap { sample ->
                listOf(sample.sent - 1, sample.sent, sample.received, sample.received + 1,
                    sample.received + 5_000_000_000, sample.received + 5_000_000_001)
            } + listOf(0L, Long.MIN_VALUE, Long.MAX_VALUE)
            for (wall in walls) for (host in hosts) {
                val linear = runCatching { clock.log(wall, host) }
                val indexed = runCatching { mapper(wall, host) }
                assertEquals(linear.getOrNull(), indexed.getOrNull(), "wall=$wall host=$host")
                assertEquals(linear.exceptionOrNull()?.javaClass, indexed.exceptionOrNull()?.javaClass)
            }
        }
        compare() // Empty and entirely invalid snapshots.
        clock.add("1", listOf(1_000_000_000L, 1_000_000_000L, wallBase + 1_000_000_000, 1_000_000_000),
            1_000_000_000, 1_040_000_001)
        compare()
        add(clock, 2_000_000_000)
        add(clock, 2_000_000_000) // Duplicate source keys: low is last, high is first.
        add(clock, 2_000_000_000, wall = wallBase + 1_999_000_000) // Valid local regression within one epoch.
        add(clock, 3_000_000_000)
        compare()
        clock.boundary(3_000_200_000)
        compare() // Closed epoch without a successor still counts as an ambiguous candidate.
        add(clock, 4_000_000_000, wall = wallBase + 2_000_000_000)
        add(clock, 5_000_000_000, wall = wallBase + 3_000_000_000)
        compare() // Overlapping wall-clock epochs.
        add(clock, 500_000_000, host = 6_000_000_000, boot = "2")
        compare() // Second boot segment.
        add(clock, 200_000_000, host = 7_000_000_000, boot = "3")
        compare() // Third boot segment: earlier boots have no time on its axis.
        val frozen = clock.logMapper()
        val expected = frozen(wallBase + 2_000_000_000, 3_000_200_000)
        clock.boundary(8_000_000_000)
        clock.clear()
        add(clock, 20_000_000_000)
        assertEquals(expected, frozen(wallBase + 2_000_000_000, 3_000_200_000))
    }

    @Test
    fun fixedMonoMapperMatchesPerRowMonoAcrossEpochsAndSurvivesClockMutation() {
        val clock = CaptureClock()
        fun compare() {
            val mapper = clock.monoMapper()
            val snapshot = clock.snapshot()
            val sources = snapshot.flatMap { sample ->
                listOf(-20_000_001L, -20_000_000L, -1L, 0L, 1L, 20_000_000L, 20_000_001L,
                    5_000_000_000L, 5_000_000_001L).map { sample.mono + it }
            } + listOf(0L, Long.MIN_VALUE, Long.MAX_VALUE, 1_500_000_000)
            val hosts = snapshot.flatMap { sample ->
                listOf(sample.sent - 1, sample.sent, sample.received, sample.received + 1, sample.received + 5_000_000_001)
            } + listOf(0L, Long.MAX_VALUE)
            val hints = (snapshot.map { it.epoch }.distinct() + clock.currentEpoch() + 99).map<Int, Int?> { it } + null
            for (source in sources) for (host in hosts) for (hint in hints) {
                val linear = runCatching { clock.mono(source, host, hint) }
                val indexed = runCatching { mapper(source, host, hint) }
                assertEquals(linear.getOrNull(), indexed.getOrNull(), "source=$source host=$host hint=$hint")
                assertEquals(linear.exceptionOrNull()?.javaClass, indexed.exceptionOrNull()?.javaClass)
            }
        }
        compare()
        add(clock, 1_000_000_000)
        add(clock, 2_000_000_000, mono = 1_999_800_000)
        add(clock, 2_000_000_000)
        compare()
        clock.boundary(2_000_200_000)
        add(clock, 3_000_000_000)
        compare()
        add(clock, 500_000_000, host = 6_000_000_000, boot = "2")
        compare()
        val frozen = clock.monoMapper()
        val expected = frozen(2_500_000_000, 3_000_200_000, null)
        clock.clear()
        add(clock, 20_000_000_000)
        assertEquals(expected, frozen(2_500_000_000, 3_000_200_000, null))
    }

    @Test
    fun maximumHistoryIndexReadsTwoSamplesPerRowAndPreservesInsertionOrderExtrema() {
        val points = List(4096) { i ->
            val wall = (i * 37L % 1024) * 1_000_000
            ClockSample(0, "1", i.toLong(), i.toLong(), wall, i.toLong(), 0, 0, 0)
        }
        var reads = 0L
        val counted = object : AbstractList<ClockSample>() {
            override val size = points.size
            override fun get(index: Int): ClockSample { reads++; return points[index] }
        }
        val index = ClockSourceIndex(counted)
        assertTrue(reads < 4096L * 100, "bounded one-time construction: $reads")
        reads = 0
        for (i in 0 until 100_000) {
            val wall = (i % 1026L - 1) * 1_000_000
            val low = index.low(wall)
            val high = index.high(wall)
            if (i < 1026) {
                assertEquals(points.lastOrNull { it.wall <= wall } ?: points.first(), low)
                assertEquals(points.firstOrNull { it.wall >= wall } ?: points.last(), high)
            }
        }
        assertEquals(200_000, reads) // No per-row traversal of the 4,096 source samples.
    }

    @Test
    fun epochRangeIndexKeepsOverlapOrderAndPrunesThousandsOfUnsupportedEpochs() {
        val ranges = List(4096) { i -> ((i * 37L % 4096) * 1000) to ((i * 37L % 4096) * 1000 + 100) }
        val index = ClockEpochIndex(ranges)
        for (i in 0 until 100_000) {
            val source = (i % 4096L) * 1000 + 50
            val matches = index.matches(source)
            assertEquals(1, matches.size)
            assertEquals(source / 1000, matches.single() * 37L % 4096)
        }
        val overlap = listOf(0L to 10L, Long.MIN_VALUE to Long.MAX_VALUE, 4L to 6L,
            3L to 2L, Long.MAX_VALUE to Long.MAX_VALUE)
        val tree = ClockEpochIndex(overlap)
        for (source in listOf(Long.MIN_VALUE, -1, 0, 2, 3, 4, 5, 6, 10, 11, Long.MAX_VALUE)) {
            assertEquals(overlap.indices.filter { source >= overlap[it].first && source <= overlap[it].second }, tree.matches(source))
        }
        assertTrue(ClockEpochIndex(emptyList()).matches(0).isEmpty())
    }

    @Test
    fun manyEpochMapperMatchesLinearCandidatesAndPreservesExtremeRoundTripExceptions() {
        val clock = CaptureClock()
        for (i in 0 until 4096) {
            clock.boundary(i * 100_000_000L)
            add(clock, 1_000_000_000 + i * 100_000_000L, host = 1_000_000_000 + i * 100_000_000L)
        }
        val mapper = clock.logMapper()
        for (sample in clock.snapshot().filterIndexed { i, _ -> i % 512 == 0 || i == 4095 }) {
            for (delta in listOf(-20_000_001L, -20_000_000L, 0L, 20_000_000L, 20_000_001L)) {
                for (host in listOf(sample.received, sample.received + 1, sample.received + 5_000_000_001)) {
                    assertEquals(clock.log(sample.wall + delta, host), mapper(sample.wall + delta, host))
                }
            }
        }
        val sample = clock.snapshot().last()
        repeat(10_000) { i ->
            assertNull(mapper(sample.wall + 10_000_000_000 + i, sample.received).elapsed)
        }
        val overflow = CaptureClock()
        overflow.add("1", listOf(Long.MAX_VALUE - 10, 0L, 0L, Long.MAX_VALUE - 10), 0, 0)
        val overflowMapper = overflow.logMapper()
        assertEquals(overflow.log(10, 0), overflowMapper(10, 0))
        kotlin.test.assertFailsWith<ArithmeticException> { overflow.log(11, 0) }
        kotlin.test.assertFailsWith<ArithmeticException> { overflowMapper(11, 0) }
        val extreme = CaptureClock()
        assertTrue(extreme.add("1", listOf(0L, 0L, 0L, 0L), Long.MIN_VALUE, 0))
        val frozen = extreme.logMapper()
        for (source in listOf(Long.MIN_VALUE, 0, Long.MAX_VALUE)) {
            kotlin.test.assertFailsWith<IllegalArgumentException> { extreme.log(source, 0) }
            kotlin.test.assertFailsWith<IllegalArgumentException> { frozen(source, 0) }
        }
    }

    @Test
    fun rebootStartsItsOwnExactSegmentAfterTheHostGapEvenWithAWideFirstRead() {
        val clock = CaptureClock()
        add(clock, 5_000_000_000, host = 10_000_000_000)
        val oldEpoch = clock.currentEpoch()
        // Too wide to map sources, but the new boot id alone opens the boot's own epoch and segment.
        assertFalse(clock.add("2", listOf(100_000_000, 100_000_000, 1_700_000_100_000_000_000, 103_000_000),
            13_000_000_000, 13_000_200_000))
        val epoch = clock.currentEpoch()
        assertTrue(epoch > oldEpoch)
        assertEquals(1, clock.segment())
        add(clock, 200_000_000, host = 13_100_000_000, boot = "2")
        assertEquals(epoch, clock.currentEpoch()) // The first good read compares only with reads of the same boot.
        val mapped = clock.video(250_000, 13_200_000_000)
        assertEquals(250_050_000, mapped.elapsed)
        // Placed by the host gap after the old boot's last read (13.0 s - 10.0002 s): ordering only.
        assertEquals(250_050_000 + 2_999_800_000 - 101_500_000, mapped.sequence)
        assertTrue(mapped.uncertainty < 20_000_000)
        val old = clock.video(5_000_000, 13_200_000_000, oldEpoch)
        assertEquals(5_000_050_000, old.elapsed)
        assertNull(old.sequence)
        assertEquals(Long.MAX_VALUE, old.uncertainty)
        assertEquals(0, clock.video(5_000_000, 13_200_000_000, oldEpoch, 0).sequence)
    }
}

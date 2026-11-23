package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.AudioStreamMeta
import io.github.shinma06.arb.model.AvOffsetStatus
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.CatalogFormatVersion
import io.github.shinma06.arb.model.DiscontinuityEvent
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.model.LogCompleteness
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.LogcatStreamMeta
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.SaveJobState
import io.github.shinma06.arb.model.SaveJobView
import io.github.shinma06.arb.model.StreamStatus
import io.github.shinma06.arb.model.StreamsSummary
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.TimestampSemantics
import io.github.shinma06.arb.model.UnanchoredSummary
import io.github.shinma06.arb.model.UncertaintyReason
import io.github.shinma06.arb.model.VideoStreamMeta
import io.github.shinma06.arb.shm.ShmFrameMeta
import io.github.shinma06.arb.shm.ShmPixelFormat
import org.junit.jupiter.api.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ArbViewModelsTest {
    private fun sampleSummary(
        isLegacy: Boolean = false,
        truncatedCount: Long = 0L,
        completeness: LogCompleteness = LogCompleteness.UNVERIFIED,
    ): TimelineSummaryResponse = TimelineSummaryResponse(
        bundleId = "bundle-1",
        path = "/tmp/bundle-1",
        format = if (isLegacy) BundleFormat.LEGACY_SCHEMA4 else BundleFormat.V2,
        isLegacy = isLegacy,
        createdAtUtc = "2026-11-23T00:00:00Z",
        provenance = Provenance.Original,
        device = null,
        streams = StreamsSummary(
            video = VideoStreamMeta(
                status = StreamStatus.PRESENT,
                frameCount = 90L,
                tailMayBeIncomplete = false,
                semantics = TimestampSemantics.VIDEO_ENCODER_INPUT_ESTIMATE,
            ),
            audio = AudioStreamMeta(
                status = StreamStatus.PARTIAL,
                avOffsetStatus = AvOffsetStatus.UNCALIBRATED,
                avOffsetMs = 0L,
            ),
            logcat = LogcatStreamMeta(
                status = StreamStatus.PRESENT,
                completeness = completeness,
                overflowEvents = if (completeness == LogCompleteness.OVERFLOW_DETECTED) 3L else 0L,
            ),
            input = null,
        ),
        unanchoredSummary = UnanchoredSummary(includedCount = 2L, truncatedCount = truncatedCount),
        epochs = emptyList(),
        frameCount = 90,
        keyframeCount = 3,
        logMainCount = 10,
        logUnanchoredCount = 2,
        tapMainCount = 0,
        tapUnanchoredCount = 0,
        discontinuityCount = 1,
        videoPath = "/tmp/bundle-1/video.mp4",
        audioPath = null,
    )

    @Test
    fun approximatePreviewBadgeTurnsOnDuringScrubAndClearsOnlyWhenExactSettles() {
        val initial = TimelineViewerState(
            bundleId = "bundle-1",
            path = "/tmp/bundle-1",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "lease-1",
            summary = sampleSummary(),
            frameCount = 90L,
        )
        assertFalse(initial.showApproximatePreviewBadge)
        assertNull(initial.approximateBadgeText)

        // Scrub publishes approximate keyframe preview -> badge MUST appear.
        val scrubbed = TimelineViewerReducer.onFramePublished(
            initial,
            ShmFrameMeta(
                generation = 1L,
                frameIndex = 30L,
                isApproximate = true,
                width = 448,
                height = 256,
                stride = 448 * 4,
                pixelFormat = ShmPixelFormat.RGBA8,
                boottimeNs = 1_000L,
            ),
        )
        assertTrue(scrubbed.showApproximatePreviewBadge)
        assertEquals(APPROXIMATE_PREVIEW_BADGE_TEXT, scrubbed.approximateBadgeText)

        // Exact seek_frame publishes settled frame -> badge clears.
        val settled = TimelineViewerReducer.onFramePublished(
            scrubbed,
            ShmFrameMeta(
                generation = 2L,
                frameIndex = 43L,
                isApproximate = false,
                width = 448,
                height = 256,
                stride = 448 * 4,
                pixelFormat = ShmPixelFormat.RGBA8,
                boottimeNs = 1_430L,
            ),
        )
        assertFalse(settled.showApproximatePreviewBadge)
        assertNull(settled.approximateBadgeText)
        assertEquals(43L, settled.currentFrameIndex)
    }

    @Test
    fun storagePressureStopBannerIsStrictlyDistinctFromConnectionLost() {
        val pressureStatus = SystemStatusResponse(
            pid = 100L,
            uptimeMs = 5_000L,
            capture = CaptureStatusView(
                active = false,
                connected = false,
                serial = "emulator-5554",
                bootId = "boot-1",
                storagePressureStopped = true,
                videoFrames = 30L,
                audioFrames = 0L,
                logRecords = 10L,
                tapRecords = 0L,
                discontinuities = 1L,
                staticHoldEvents = 0L,
            ),
            ringChunkCount = 2L,
            ringTotalBytes = 1024L,
            ringPinnedChunkCount = 1L,
            activeViewports = 0L,
            activeSaveJobs = 0L,
            recovery = RecoverySummaryView(),
        )
        val statePressure = CaptureBarReducer.fromStatus(CaptureBarState(), pressureStatus)
        assertEquals(listOf(CaptureBannerKind.STORAGE_PRESSURE_STOP), statePressure.banners.map { it.kind })

        val disconnectedStatus = pressureStatus.copy(
            capture = pressureStatus.capture.copy(
                active = true,
                connected = false,
                storagePressureStopped = false,
                staticHoldEvents = 2L,
            ),
        )
        val stateDisconnected = CaptureBarReducer.fromStatus(
            CaptureBarState(),
            disconnectedStatus,
            liveDiscontinuities = listOf(
                DiscontinuityEvent(kind = DiscontinuityKind.STREAM_GAP, pcContStartNs = 100L),
            ),
        )
        assertEquals(
            listOf(CaptureBannerKind.CONNECTION_LOST, CaptureBannerKind.STREAM_GAP, CaptureBannerKind.STATIC_HOLD),
            stateDisconnected.banners.map { it.kind },
        )
    }

    @Test
    fun unanchoredLogsStayStrictlySeparatedAndSurfaceTruncatedNotice() {
        val state = TimelineViewerState(
            bundleId = "bundle-1",
            path = "/tmp/bundle-1",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "lease-1",
            summary = sampleSummary(truncatedCount = 15L),
            frameCount = 90L,
        )
        val mainLog = LogRecord(
            seq = 1L,
            pcIngestContNs = 100L,
            alignment = TimeAlignment.Anchored("boot-1", 1_000L, 10L, TimestampSemantics.LOGCAT_HEADER_WALL),
            bucket = RecordBucket.MAIN,
            message = "main-entry",
        )
        val unanchoredLog = LogRecord(
            seq = 2L,
            pcIngestContNs = 200L,
            alignment = TimeAlignment.Unanchored(200L, UncertaintyReason.CLOCK_SYNC_STALE),
            bucket = RecordBucket.UNANCHORED,
            message = "unanchored-entry",
        )

        val updated = TimelineViewerReducer.withBucketedRecords(
            state = state,
            mainLogs = listOf(mainLog, unanchoredLog), // reducer enforces strict filtering!
            unanchoredLogs = listOf(mainLog, unanchoredLog),
            mainTaps = emptyList(),
            unanchoredTaps = emptyList(),
        )
        assertEquals(listOf(mainLog), updated.mainLogs)
        assertEquals(listOf(unanchoredLog), updated.unanchoredLogs)
        assertNotNull(updated.unanchoredTruncatedNotice)
        assertTrue(updated.unanchoredTruncatedNotice!!.contains("15"))
    }

    @Test
    fun provenanceAndDiscontinuityFormattersStayHonest() {
        val formatted = ProvenanceFormatter.format(sampleSummary(completeness = LogCompleteness.UNVERIFIED))
        assertTrue(formatted.videoSemanticsLine.contains("エンコーダ入力時刻推定"))
        assertTrue(formatted.audioOffsetLine.contains("Partial"))
        assertTrue(formatted.audioOffsetLine.contains("未校正"))
        assertTrue(formatted.logcatCompletenessLine.contains("未確認 (UNVERIFIED)"))

        val overflowFormatted = ProvenanceFormatter.format(sampleSummary(completeness = LogCompleteness.OVERFLOW_DETECTED))
        assertTrue(overflowFormatted.logcatCompletenessLine.contains("オーバーフロー検出"))

        for (kind in DiscontinuityKind.entries) {
            val label = DiscontinuityFormatter.label(kind)
            assertTrue(label.contains(kind.name), "Expected label '$label' to contain ${kind.name}")
        }
    }

    @Test
    fun failedSaveRecoveryStateAndCatalogLegacyBadgesWork() {
        val failedJob = SaveJobView(
            jobId = "job-1",
            state = SaveJobState.FAILED_RETRYABLE,
            destinationPath = "/readonly/dest",
            pinnedChunkIds = listOf("chunk-1", "chunk-2"),
            lastError = "permission denied",
            createdAtUtc = "2026-11-23T00:00:00Z",
            updatedAtUtc = "2026-11-23T00:00:01Z",
        )
        val s1 = FailedSaveRecoveryReducer.withJobs(FailedSaveRecoveryState(), listOf(failedJob))
        assertTrue(s1.hasRecoverableFailures)
        assertEquals(1, s1.retryableJobs.size)

        val s2 = FailedSaveRecoveryReducer.markBusy(s1, "job-1")
        assertEquals("job-1", s2.busyJobId)

        val completedJob = failedJob.copy(state = SaveJobState.COMPLETED, pinnedChunkIds = emptyList(), lastError = null)
        val s3 = FailedSaveRecoveryReducer.applyJobUpdated(s2, completedJob, "retarget succeeded")
        assertFalse(s3.hasRecoverableFailures)
        assertNull(s3.busyJobId)

        val catalog = BundleCatalogState.fromEntries(
            listOf(
                BundleEntryView(
                    bundleId = "legacy-1",
                    path = "/old/replay-1",
                    formatVersion = CatalogFormatVersion.LEGACY_RO,
                    state = BundleLifecycleState.ACTIVE,
                    createdAtUtc = "2024-01-01T00:00:00Z",
                    activeLeases = 1,
                ),
                BundleEntryView(
                    bundleId = "v2-1",
                    path = "/data/bundles/v2-1",
                    formatVersion = CatalogFormatVersion.V2,
                    state = BundleLifecycleState.TRASHED,
                    createdAtUtc = "2026-11-23T00:00:00Z",
                    activeLeases = 0,
                ),
            ),
        )
        assertEquals(LEGACY_READ_ONLY_BADGE_TEXT, catalog.items[0].formatBadge)
        assertTrue(catalog.items[0].isProtectedByLease)
        assertEquals("v2", catalog.items[1].formatBadge)
        assertTrue(catalog.items[1].canRestore)
        assertFalse(catalog.items[1].canTrash)
    }

    @Test
    fun integerFrameRangeHandlesStayClampedAndOrdered() {
        val state = TimelineViewerState(
            bundleId = "bundle-1",
            path = "/tmp/bundle-1",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "lease-1",
            summary = sampleSummary(),
            frameCount = 90L,
            currentFrameIndex = 0L,
            inFrameIndex = 0L,
            outFrameIndex = 89L,
        )
        val steppedBack = TimelineViewerReducer.stepFrame(state, -1L)
        assertEquals(0L, steppedBack.currentFrameIndex)
        val steppedFwd = TimelineViewerReducer.stepFrame(state, +1L)
        assertEquals(1L, steppedFwd.currentFrameIndex)

        val movedInPastOut = TimelineViewerReducer.setInFrame(state.copy(outFrameIndex = 20L), 50L)
        assertEquals(50L, movedInPastOut.inFrameIndex)
        assertEquals(50L, movedInPastOut.outFrameIndex)

        val movedOutBeforeIn = TimelineViewerReducer.setOutFrame(movedInPastOut, 10L)
        assertEquals(10L, movedOutBeforeIn.inFrameIndex)
        assertEquals(10L, movedOutBeforeIn.outFrameIndex)
    }
}

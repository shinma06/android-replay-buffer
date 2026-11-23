package io.github.shinma06.arb.ui

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
import io.github.shinma06.arb.state.APPROXIMATE_PREVIEW_BADGE_TEXT
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.CaptureBannerKind
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.FailedSaveRecoveryReducer
import io.github.shinma06.arb.state.FailedSaveRecoveryState
import io.github.shinma06.arb.state.LEGACY_READ_ONLY_BADGE_TEXT
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.UNANCHORED_TAB_TITLE
import org.junit.jupiter.api.Test
import java.awt.image.BufferedImage
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ArbUiComponentsTest {
    @Test
    fun videoViewportCanvasTogglesMandatoryApproximateBadge() {
        val canvas = VideoViewportCanvas()
        assertFalse(canvas.isApproximateBadgeVisible())

        val img = BufferedImage(4, 4, BufferedImage.TYPE_INT_ARGB)
        canvas.updateFrame(
            ShmFrameMeta(1L, 10L, true, 4, 4, 16, ShmPixelFormat.RGBA8, 100L),
            img,
        )
        assertTrue(canvas.isApproximateBadgeVisible())
        assertEquals(APPROXIMATE_PREVIEW_BADGE_TEXT, canvas.approximateBadgeText())

        canvas.updateFrame(
            ShmFrameMeta(2L, 12L, false, 4, 4, 16, ShmPixelFormat.RGBA8, 200L),
            img,
        )
        assertFalse(canvas.isApproximateBadgeVisible())
    }

    @Test
    fun timelineViewerPanelSeparatesMainAndUnanchoredLogsAndShowsTruncatedBanner() {
        val panel = TimelineViewerPanel(
            onScrub = {},
            onSeekExact = {},
            onTrimExport = { _, _ -> },
            onPngExport = { _, _ -> },
        )
        val summary = TimelineSummaryResponse(
            bundleId = "b-ui-1",
            path = "/tmp/b-ui-1",
            format = BundleFormat.V2,
            isLegacy = false,
            provenance = Provenance.Original,
            streams = StreamsSummary(
                video = VideoStreamMeta(StreamStatus.PRESENT, 60L, false, TimestampSemantics.VIDEO_ENCODER_INPUT_ESTIMATE),
                audio = AudioStreamMeta(StreamStatus.PARTIAL, AvOffsetStatus.UNCALIBRATED, 0L),
                logcat = LogcatStreamMeta(StreamStatus.PRESENT, LogCompleteness.UNVERIFIED, 0L),
            ),
            unanchoredSummary = UnanchoredSummary(includedCount = 1L, truncatedCount = 4L),
            epochs = emptyList(),
            frameCount = 60,
            keyframeCount = 2,
            logMainCount = 2,
            logUnanchoredCount = 1,
            tapMainCount = 0,
            tapUnanchoredCount = 0,
            discontinuityCount = 2,
        )
        val state = TimelineViewerState(
            bundleId = "b-ui-1",
            path = "/tmp/b-ui-1",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "lease-1",
            summary = summary,
            frameCount = 60L,
            currentFrameMeta = ShmFrameMeta(1L, 5L, true, 4, 4, 16, ShmPixelFormat.RGBA8, 50L),
            discontinuities = listOf(
                DiscontinuityEvent(kind = DiscontinuityKind.DEVICE_REBOOT, pcContStartNs = 10L),
                DiscontinuityEvent(kind = DiscontinuityKind.STORAGE_PRESSURE_STOP, pcContStartNs = 20L),
            ),
            mainLogs = listOf(
                LogRecord(1L, pcIngestContNs = 10L, alignment = TimeAlignment.Anchored("b", 10L, 1L, TimestampSemantics.LOGCAT_HEADER_WALL), bucket = RecordBucket.MAIN, message = "m1"),
                LogRecord(2L, pcIngestContNs = 20L, alignment = TimeAlignment.Anchored("b", 20L, 1L, TimestampSemantics.LOGCAT_HEADER_WALL), bucket = RecordBucket.MAIN, message = "m2"),
            ),
            unanchoredLogs = listOf(
                LogRecord(3L, pcIngestContNs = 30L, alignment = TimeAlignment.Unanchored(30L, UncertaintyReason.CLOCK_SYNC_STALE), bucket = RecordBucket.UNANCHORED, message = "u1"),
            ),
        )

        panel.renderState(state, image = null)
        assertTrue(panel.videoCanvas.isApproximateBadgeVisible())
        assertEquals(2, panel.discontinuityBar.renderedEventCount())
        assertEquals(2, panel.mainLogRowCount())
        assertEquals(1, panel.unanchoredLogRowCount())
        assertTrue(panel.logsTabbedPane.getTitleAt(1).startsWith(UNANCHORED_TAB_TITLE))
        assertTrue(panel.isUnanchoredTruncatedBannerVisible())
    }

    @Test
    fun captureBarCatalogAndFailedRecoveryPanelsRenderHonestStates() {
        val captureBar = CaptureBarPanel({}, {}, {}, {})
        val status = SystemStatusResponse(
            pid = 7L,
            uptimeMs = 100L,
            capture = CaptureStatusView(
                active = false,
                connected = false,
                serial = "dev-1",
                storagePressureStopped = true,
                videoFrames = 10L,
                audioFrames = 0L,
                logRecords = 5L,
                tapRecords = 0L,
                discontinuities = 1L,
                staticHoldEvents = 0L,
            ),
            ringChunkCount = 1L,
            ringTotalBytes = 4096L,
            ringPinnedChunkCount = 1L,
            activeViewports = 0L,
            activeSaveJobs = 1L,
            recovery = RecoverySummaryView(),
        )
        captureBar.render(CaptureBarReducer.fromStatus(CaptureBarState(), status))
        assertEquals(listOf(CaptureBannerKind.STORAGE_PRESSURE_STOP), captureBar.visibleBannerKinds())

        val recovery = FailedSaveRecoveryPanel({}, { _, _ -> }, {})
        val job = SaveJobView(
            jobId = "job-err-1",
            state = SaveJobState.FAILED_RETRYABLE,
            destinationPath = "/bad/path",
            pinnedChunkIds = listOf("c1"),
            lastError = "read-only fs",
            createdAtUtc = "2026-11-23T00:00:00Z",
            updatedAtUtc = "2026-11-23T00:00:00Z",
        )
        recovery.render(FailedSaveRecoveryReducer.withJobs(FailedSaveRecoveryState(), listOf(job)))
        assertTrue(recovery.isVisible)
        assertEquals(1, recovery.displayedRetryableCount())

        val catalog = BundleCatalogPanel({}, {}, {}, {})
        catalog.render(
            BundleCatalogState.fromEntries(
                listOf(
                    BundleEntryView("leg-1", "/old", CatalogFormatVersion.LEGACY_RO, BundleLifecycleState.ACTIVE, null, null, "2024-01-01", 1),
                    BundleEntryView("v2-1", "/new", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, null, "2026-11-23", 0),
                ),
            ),
        )
        assertEquals(2, catalog.rowCount())
        assertEquals(LEGACY_READ_ONLY_BADGE_TEXT, catalog.badgeAtRow(0))
        assertEquals("v2", catalog.badgeAtRow(1))
    }
}

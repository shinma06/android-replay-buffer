package io.github.shinma06.arb.ui

import io.github.shinma06.arb.ide.SourceJumpResolver
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.AudioStreamMeta
import io.github.shinma06.arb.model.AvOffsetStatus
import io.github.shinma06.arb.model.BundleEntryView
import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.CaptureStatusView
import io.github.shinma06.arb.model.CatalogFormatVersion
import io.github.shinma06.arb.model.CrashMarker
import io.github.shinma06.arb.model.DiscontinuityEvent
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.model.EpochSpan
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.LogCompleteness
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.LogcatStreamMeta
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.RecoverySummaryView
import io.github.shinma06.arb.model.SaveJobState
import io.github.shinma06.arb.model.SaveJobView
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.model.StreamStatus
import io.github.shinma06.arb.model.StreamsSummary
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.TimestampSemantics
import io.github.shinma06.arb.model.UnanchoredSummary
import io.github.shinma06.arb.model.UncertaintyReason
import io.github.shinma06.arb.model.VideoStreamMeta
import io.github.shinma06.arb.shm.ShmFrameMeta
import io.github.shinma06.arb.shm.ShmPixelFormat
import io.github.shinma06.arb.state.APPROXIMATE_PREVIEW_BADGE_TEXT
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.CaptureBannerKind
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.ExportCompletionInfo
import io.github.shinma06.arb.state.FailedSaveRecoveryReducer
import io.github.shinma06.arb.state.FailedSaveRecoveryState
import io.github.shinma06.arb.state.LEGACY_READ_ONLY_BADGE_TEXT
import io.github.shinma06.arb.state.SourceJumpTarget
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.UNANCHORED_TAB_TITLE
import org.junit.jupiter.api.Test
import java.awt.image.BufferedImage
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ArbUiComponentsTest {
    @Test
    fun videoViewportCanvasTogglesMandatoryApproximateBadgeAndTapRipples() {
        val canvas = VideoViewportCanvas()
        assertFalse(canvas.isApproximateBadgeVisible())

        val img = BufferedImage(40, 80, BufferedImage.TYPE_INT_ARGB)
        canvas.updateFrame(
            ShmFrameMeta(1L, 10L, true, 40, 80, 160, ShmPixelFormat.RGBA8, 100L),
            img,
        )
        assertTrue(canvas.isApproximateBadgeVisible())
        assertEquals(APPROXIMATE_PREVIEW_BADGE_TEXT, canvas.approximateBadgeText())

        canvas.updateFrame(
            ShmFrameMeta(2L, 12L, false, 40, 80, 160, ShmPixelFormat.RGBA8, 200L),
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
        assertFalse(panel.multiTrackDock.isSelectTapRangeVisible(), "Tap preset must stay hidden when taps == 0")
        assertFalse(panel.multiTrackDock.isSelectAroundCrashVisible(), "Crash preset must stay hidden when crashes == 0")
        assertTrue(panel.multiTrackDock.isTrashOriginalEnabled(), "v2 bundle allows non-destructive trash-original")
        assertEquals(ArbFormatters.EXPORT_FULL_LABEL, panel.exportButtonText())
    }

    @Test
    fun captureBarCockpitDualSaveActionsSecondsNotationAndCatalogFiltersWork() {
        var saveAndOpenSecs = -1L
        var saveOnlySecs = -1L
        var screenshotCalled = false
        var saveLogsSecs = -1L

        val captureBar = CaptureBarPanel(
            onStartCapture = {},
            onStopCapture = {},
            onSaveNow = {},
            onRefresh = {},
            onSaveAndOpen = { saveAndOpenSecs = it },
            onSaveOnly = { saveOnlySecs = it },
            onScreenshot = { screenshotCalled = true },
            onSaveLogs = { saveLogsSecs = it },
        )

        val healthyStatus = SystemStatusResponse(
            pid = 7L,
            uptimeMs = 100L,
            capture = CaptureStatusView(
                active = true,
                connected = true,
                serial = "dev-1",
                storagePressureStopped = false,
                videoFrames = 1_350L, // 45s @ 30fps
                audioFrames = 0L,
                logRecords = 5L,
                tapRecords = 0L,
                discontinuities = 0L,
                staticHoldEvents = 0L,
            ),
            ringChunkCount = 4L,
            ringTotalBytes = 100L * 1024L * 1024L,
            ringPinnedChunkCount = 0L,
            activeViewports = 0L,
            activeSaveJobs = 0L,
            recovery = RecoverySummaryView(),
        )

        // Healthy -> Tier-2 banner container hidden, strict seconds CTA, SaveAndOpen default
        val stateDefault = CaptureBarReducer.fromStatus(
            CaptureBarState(),
            healthyStatus,
            config = ArbConfig(saveWindowSecs = 300, ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_AND_OPEN),
        )
        captureBar.render(stateDefault)
        assertFalse(captureBar.isBannerContainerVisible(), "Healthy cockpit must hide Tier-2 banner container")
        assertEquals("REC", captureBar.statusBadgeDisplayText())
        assertEquals("直前 300s を保存して開く", captureBar.primarySaveButtonText())
        assertEquals("保存のみ", captureBar.secondarySaveButtonText())
        assertTrue(captureBar.ringMeterDisplayText().startsWith("45s / 300s"))
        assertFalse(ArbFormatters.containsEmoji(captureBar.primarySaveButtonText()))

        captureBar.clickPrimarySave()
        assertEquals(300L, saveAndOpenSecs)
        captureBar.clickSecondarySave()
        assertEquals(300L, saveOnlySecs)
        captureBar.clickScreenshot()
        assertTrue(screenshotCalled)
        captureBar.clickSaveLogs()
        assertEquals(300L, saveLogsSecs)

        // Swapped default action (SaveOnly primary, SaveAndOpen secondary)
        saveAndOpenSecs = -1L
        saveOnlySecs = -1L
        val stateSwapped = CaptureBarReducer.fromStatus(
            CaptureBarState(),
            healthyStatus.copy(capture = healthyStatus.capture.copy(active = false, storagePressureStopped = true)),
            config = ArbConfig(saveWindowSecs = 45, ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY),
        )
        captureBar.render(stateSwapped)
        assertTrue(captureBar.isBannerContainerVisible())
        assertEquals(listOf(CaptureBannerKind.STORAGE_PRESSURE_STOP), captureBar.visibleBannerKinds())
        assertEquals("直前 45s を保存のみ", captureBar.primarySaveButtonText())
        assertEquals("保存して開く", captureBar.secondarySaveButtonText())
        captureBar.clickPrimarySave()
        assertEquals(45L, saveOnlySecs)
        captureBar.clickSecondarySave()
        assertEquals(45L, saveAndOpenSecs)

        // FailedSaveRecoveryPanel
        val recovery = FailedSaveRecoveryPanel({}, { _, _ -> }, {})
        assertFalse(recovery.isVisible, "Healthy recovery panel must stay hidden")
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

        // Filterable Library List
        val catalog = BundleCatalogPanel({}, {}, {}, {})
        catalog.render(
            BundleCatalogState.fromEntries(
                entries = listOf(
                    BundleEntryView("leg-1", "/old", CatalogFormatVersion.LEGACY_RO, BundleLifecycleState.ACTIVE, null, null, "2024-01-01", 1),
                    BundleEntryView("v2-trim", "/new-trim", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, "v2-orig", "2026-11-23", 0),
                    BundleEntryView("v2-crash", "/new-crash", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, null, "2026-11-23", 0),
                    BundleEntryView("v2-trash", "/new-trash", CatalogFormatVersion.V2, BundleLifecycleState.TRASHED, "2026-11-23", null, "2026-11-23", 0),
                ),
                crashBundleIds = setOf("v2-crash"),
            ),
        )
        assertEquals(3, catalog.rowCount(), "ALL filter lists active non-trashed bundles")
        assertEquals(LEGACY_READ_ONLY_BADGE_TEXT, catalog.badgeAtRow(0))
        assertEquals("v2", catalog.badgeAtRow(1))

        catalog.selectFilter(BundleFilter.TRIMMED)
        assertEquals(1, catalog.rowCount())
        assertEquals("v2-trim", catalog.itemAtRow(0).bundleId)

        catalog.selectFilter(BundleFilter.CRASH)
        assertEquals(1, catalog.rowCount())
        assertEquals("v2-crash", catalog.itemAtRow(0).bundleId)

        catalog.selectFilter(BundleFilter.TRASH)
        assertEquals(1, catalog.rowCount())
        assertEquals("v2-trash", catalog.itemAtRow(0).bundleId)
    }

    @Test
    fun integratedStudioWiresSnapButtonsPresetsSourceJumpLegacyGuardAndExportToast() {
        var lastRange: Triple<Long, Long, Long>? = null
        val jumpedTargets = mutableListOf<SourceJumpTarget>()
        val fakeResolver = object : SourceJumpResolver {
            override fun resolveAndNavigate(target: SourceJumpTarget): Boolean {
                jumpedTargets.add(target)
                return true
            }
        }

        val panel = TimelineViewerPanel(
            onScrub = {},
            onSeekExact = {},
            onTrimExport = { _, _ -> },
            onPngExport = { _, _ -> },
            onRangeChanged = { i, o, p -> lastRange = Triple(i, o, p) },
            onSourceJump = { fakeResolver.resolveAndNavigate(it) },
        )

        val base = 10_000_000_000L
        val summary = TimelineSummaryResponse(
            bundleId = "b-studio",
            path = "/tmp/b-studio",
            format = BundleFormat.V2,
            isLegacy = false,
            provenance = Provenance.Original,
            streams = StreamsSummary(
                video = VideoStreamMeta(StreamStatus.PRESENT, 100L, false, TimestampSemantics.VIDEO_ENCODER_INPUT_ESTIMATE),
                audio = AudioStreamMeta(StreamStatus.PRESENT, AvOffsetStatus.CALIBRATED, 0L),
                logcat = LogcatStreamMeta(StreamStatus.PRESENT, LogCompleteness.VERIFIED_NO_OVERFLOW, 0L),
            ),
            unanchoredSummary = UnanchoredSummary(0L, 0L),
            epochs = listOf(EpochSpan("boot-1", base, base + 10_000_000_000L)),
            frameCount = 100,
            keyframeCount = 4,
            logMainCount = 2,
            logUnanchoredCount = 0,
            tapMainCount = 1,
            tapUnanchoredCount = 0,
            discontinuityCount = 0,
        )

        val state = TimelineViewerState(
            bundleId = "b-studio",
            path = "/tmp/b-studio",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "l-studio",
            summary = summary,
            frameCount = 100L,
            currentFrameIndex = 20L,
            inFrameIndex = 10L,
            outFrameIndex = 50L,
            screens = listOf(
                ScreenSpan(base, base + 5_000_000_000L, 0L, 50L, "com.example.app", "com.example.app.LoginActivity", "LoginActivity"),
            ),
            crashes = listOf(
                CrashMarker(base + 4_000_000_000L, 40L, "com.example.app", "com.example.app", 99, "java.lang.IllegalStateException", "boom", 50L),
            ),
            mainTaps = listOf(
                TapRecord(1L, pcIngestContNs = base + 2_000_000_000L, alignment = TimeAlignment.Anchored("boot-1", base + 2_000_000_000L, 1L, TimestampSemantics.INPUT_EVENT_TIME), bucket = RecordBucket.MAIN, action = "DOWN", x = 120.0, y = 240.0),
            ),
            mainLogs = listOf(
                LogRecord(2L, pcIngestContNs = base + 2_100_000_000L, alignment = TimeAlignment.Anchored("boot-1", base + 2_100_000_000L, 1L, TimestampSemantics.LOGCAT_DEVICE_READ_BOOTTIME), bucket = RecordBucket.MAIN, level = "E", tag = "Shop", message = "at com.example.app.Checkout.run(Checkout.kt:84)"),
            ),
            exportCompletion = ExportCompletionInfo(
                summaryText = "トリムを書き出しました: b-trimmed",
                outputPath = "/tmp/b-trimmed",
                bundleId = "b-trimmed",
                isTrimBundle = true,
            ),
        )

        panel.renderState(state, image = null)

        assertTrue(panel.visibleHonestyChipLabels().isEmpty(), "Healthy bundle has 0 honesty chips")
        assertTrue(panel.exportButtonText().startsWith("選択範囲を書き出し ("), "Sub-range CTA shows seconds duration")
        assertTrue(panel.multiTrackDock.isSelectTapRangeVisible())
        assertTrue(panel.multiTrackDock.isSelectAroundCrashVisible())
        assertEquals(1, panel.multiTrackDock.renderedScreenSpanCount())
        assertEquals(1, panel.multiTrackDock.renderedCrashMarkerCount())
        assertEquals(1, panel.videoCanvas.renderedTapOverlayCount())
        assertTrue(panel.isExportCompletionBarVisible())

        // Event row [In] / [Out] snap & PSI source jump
        val rows = panel.eventLogStreamPanel.syncedRows()
        val logRowIdx = rows.indexOfFirst { it.seq == 2L }
        assertTrue(logRowIdx >= 0)
        panel.eventLogStreamPanel.triggerSnapInAtRow(logRowIdx)
        assertEquals(rows[logRowIdx].frameIndex, lastRange?.first)
        panel.eventLogStreamPanel.triggerSourceJumpAtRow(logRowIdx)
        assertEquals(1, jumpedTargets.size)
        assertEquals("Checkout.kt:84", jumpedTargets[0].displayLabel)

        // Legacy RO disables trash-original checkbox
        panel.renderState(state.copy(isLegacy = true, format = BundleFormat.LEGACY_SCHEMA4), image = null)
        assertFalse(panel.multiTrackDock.isTrashOriginalEnabled())
        assertFalse(panel.multiTrackDock.isTrashOriginalSelected())
        assertTrue(panel.visibleHonestyChipLabels().contains(LEGACY_READ_ONLY_BADGE_TEXT))
    }
}

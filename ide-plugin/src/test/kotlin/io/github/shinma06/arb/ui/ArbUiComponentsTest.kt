package io.github.shinma06.arb.ui

import io.github.shinma06.arb.ide.ArbdDaemonService
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
import java.nio.file.Files
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ArbUiComponentsTest {
    /**
     * P2-06: the Studio header renders [ArbStudioController.headerSubtitle] verbatim. A bundle of
     * the current format without device info used to show its wire name `v2` there.
     */
    @Test
    fun studioHeaderSubtitleNeverFallsBackToTheWireFormatName() {
        fun subtitleFor(format: BundleFormat, device: io.github.shinma06.arb.model.DeviceInfo?): String {
            val studio = ArbStudioController(onScrub = {}, onSeekExact = {}, onTrimExport = { _, _ -> }, onPngExport = { _, _ -> })
            val summary = TimelineSummaryResponse(
                bundleId = "b-1",
                path = "/tmp/b-1",
                format = format,
                isLegacy = format != BundleFormat.V2,
                createdAtUtc = "2026-11-24T01:02:03Z",
                provenance = Provenance.Original,
                device = device,
                streams = StreamsSummary(),
                unanchoredSummary = UnanchoredSummary(0L, 0L),
                epochs = emptyList(),
                frameCount = 1,
                keyframeCount = 1,
                logMainCount = 0,
                logUnanchoredCount = 0,
                tapMainCount = 0,
                tapUnanchoredCount = 0,
                discontinuityCount = 0,
            )
            studio.renderState(
                TimelineViewerState(
                    bundleId = "b-1",
                    path = "/tmp/b-1",
                    format = format,
                    isLegacy = summary.isLegacy,
                    leaseId = "l-1",
                    summary = summary,
                    frameCount = 1L,
                ),
                image = null,
            )
            return studio.headerSubtitle
        }

        for (format in BundleFormat.entries) {
            val subtitle = subtitleFor(format, device = null)
            assertEquals("${ArbStudioController.UNKNOWN_DEVICE_LABEL} · 2026-11-24 01:02:03", subtitle)
            assertFalse(subtitle.contains(format.wire), "wire name '${format.wire}' leaked into '$subtitle'")
            assertFalse(Regex("""\bv2\b""", RegexOption.IGNORE_CASE).containsMatchIn(subtitle))
        }
        assertEquals(
            "Pixel 9 (API 36) · 2026-11-24 01:02:03",
            subtitleFor(BundleFormat.V2, io.github.shinma06.arb.model.DeviceInfo("emu", "Pixel 9", 36, true)),
        )
    }

    @Test
    fun studioControllerTogglesMandatoryApproximateBadgeAndTapRipples() {
        val studio = ArbStudioController(
            onScrub = {},
            onSeekExact = {},
            onTrimExport = { _, _ -> },
            onPngExport = { _, _ -> },
        )
        assertFalse(studio.approximatePreviewVisible)

        val img = BufferedImage(40, 80, BufferedImage.TYPE_INT_ARGB)
        val baseState = TimelineViewerState(
            bundleId = "b-approx",
            path = "/tmp/b-approx",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "l-1",
            summary = TimelineSummaryResponse(
                bundleId = "b-approx",
                path = "/tmp/b-approx",
                format = BundleFormat.V2,
                isLegacy = false,
                provenance = Provenance.Original,
                streams = StreamsSummary(),
                unanchoredSummary = UnanchoredSummary(0L, 0L),
                epochs = emptyList(),
                frameCount = 20,
                keyframeCount = 2,
                logMainCount = 0,
                logUnanchoredCount = 0,
                tapMainCount = 0,
                tapUnanchoredCount = 0,
                discontinuityCount = 0,
            ),
            frameCount = 20L,
        )
        studio.renderState(baseState, image = img)
        studio.applyFramePublished(
            ShmFrameMeta(1L, 10L, true, 40, 80, 160, ShmPixelFormat.RGBA8, 100L),
            img,
        )
        assertTrue(studio.approximatePreviewVisible)
        assertEquals(APPROXIMATE_PREVIEW_BADGE_TEXT, ArbStudioController.APPROXIMATE_BADGE_TEXT)

        studio.applyFramePublished(
            ShmFrameMeta(2L, 12L, false, 40, 80, 160, ShmPixelFormat.RGBA8, 200L),
            img,
        )
        assertFalse(studio.approximatePreviewVisible)
    }

    @Test
    fun studioControllerSeparatesMainAndUnanchoredLogsAndShowsTruncatedBanner() {
        val studio = ArbStudioController(
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

        studio.renderState(state, image = null)
        assertTrue(studio.approximatePreviewVisible)
        assertEquals(2, studio.viewerState?.discontinuities?.size)
        assertEquals(2, studio.mainLogRowCount)
        assertEquals(1, studio.unanchoredLogRowCount)
        assertTrue(studio.unanchoredTabLabel.startsWith(UNANCHORED_TAB_TITLE))
        assertTrue(studio.isUnanchoredTruncatedBannerVisible)
        assertFalse(studio.isSelectTapRangeVisible, "Tap preset must stay hidden when taps == 0")
        assertFalse(studio.isSelectAroundCrashVisible, "Crash preset must stay hidden when crashes == 0")
        assertTrue(studio.isTrashOriginalEnabled, "v2 bundle allows non-destructive trash-original")
        assertEquals(ArbFormatters.EXPORT_FULL_LABEL, studio.exportButtonLabel)
    }

    @Test
    fun toolWindowControllerCockpitSecondsNotationCatalogFiltersAndGcOnlyInTrashTab() {
        val service = ArbdDaemonService()
        val controller = ArbToolWindowController(
            project = null,
            daemonService = service,
            onOpenBundle = { _, _, _ -> },
            pollIntervalMs = 0L,
        )
        try {
            val healthyStatus = SystemStatusResponse(
                pid = 7L,
                uptimeMs = 100L,
                capture = CaptureStatusView(
                    active = true,
                    connected = true,
                    serial = "dev-1",
                    storagePressureStopped = false,
                    videoFrames = 1_350L,
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

            val stateDefault = CaptureBarReducer.fromStatus(
                CaptureBarState(),
                healthyStatus,
                config = ArbConfig(saveWindowSecs = 300, ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_AND_OPEN),
            )
            controller.applyStateForTest(capture = stateDefault)
            assertFalse(controller.captureState.hasVisibleBanners, "Healthy cockpit must hide Tier-2 banners")
            assertEquals("REC", controller.captureState.statusBadgeText)
            assertEquals("直前 300s を保存して開く", controller.captureState.primarySaveLabel)
            assertEquals("保存のみ", controller.captureState.secondarySaveLabel)
            assertTrue(controller.captureState.ringMeterText.startsWith("45s / 300s"))
            assertFalse(ArbFormatters.containsEmoji(controller.captureState.primarySaveLabel))

            // Swapped default action + storage pressure banner
            val stateSwapped = CaptureBarReducer.fromStatus(
                CaptureBarState(),
                healthyStatus.copy(capture = healthyStatus.capture.copy(active = false, storagePressureStopped = true)),
                config = ArbConfig(saveWindowSecs = 45, ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY),
            )
            controller.applyStateForTest(capture = stateSwapped)
            assertTrue(controller.captureState.hasVisibleBanners)
            assertEquals(listOf(CaptureBannerKind.STORAGE_PRESSURE_STOP), controller.captureState.banners.map { it.kind })
            assertEquals("直前 45s を保存のみ", controller.captureState.primarySaveLabel)
            assertEquals("保存して開く", controller.captureState.secondarySaveLabel)

            // Failed save recovery state
            val job = SaveJobView(
                jobId = "job-err-1",
                state = SaveJobState.FAILED_RETRYABLE,
                destinationPath = "/bad/path",
                pinnedChunkIds = listOf("c1"),
                lastError = "read-only fs",
                createdAtUtc = "2026-11-23T00:00:00Z",
                updatedAtUtc = "2026-11-23T00:00:00Z",
            )
            controller.applyStateForTest(
                recovery = FailedSaveRecoveryReducer.withJobs(FailedSaveRecoveryState(), listOf(job)),
            )
            assertEquals(1, controller.recoveryState.retryableJobs.size)

            // Filterable library list & GC visibility strictly inside TRASH tab
            controller.applyStateForTest(
                catalog = BundleCatalogState.fromEntries(
                    entries = listOf(
                        BundleEntryView("leg-1", "/old", CatalogFormatVersion.LEGACY_RO, BundleLifecycleState.ACTIVE, null, null, "2024-01-01", 1),
                        BundleEntryView("v2-trim", "/new-trim", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, "v2-orig", "2026-11-23", 0),
                        BundleEntryView("v2-crash", "/new-crash", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, null, "2026-11-23", 0),
                        BundleEntryView("v2-trash", "/new-trash", CatalogFormatVersion.V2, BundleLifecycleState.TRASHED, "2026-11-23", null, "2026-11-23", 0),
                    ),
                    crashBundleIds = setOf("v2-crash"),
                ),
            )
            assertFalse(controller.showGcAction, "GC action must be hidden on ALL tab")
            assertEquals(3, controller.displayedBundles.size)
            assertEquals(LEGACY_READ_ONLY_BADGE_TEXT, controller.displayedBundles[0].formatBadge)
            assertEquals("ARB", controller.displayedBundles[1].formatBadge)

            controller.selectFilter(BundleFilter.TRIMMED)
            javax.swing.SwingUtilities.invokeAndWait {}
            assertFalse(controller.showGcAction)
            assertEquals(1, controller.displayedBundles.size)
            assertEquals("v2-trim", controller.displayedBundles[0].bundleId)

            controller.selectFilter(BundleFilter.CRASH)
            javax.swing.SwingUtilities.invokeAndWait {}
            assertFalse(controller.showGcAction)
            assertEquals(1, controller.displayedBundles.size)
            assertEquals("v2-crash", controller.displayedBundles[0].bundleId)

            controller.selectFilter(BundleFilter.TRASH)
            javax.swing.SwingUtilities.invokeAndWait {}
            assertTrue(controller.showGcAction, "GC action must appear ONLY inside TRASH tab")
            assertEquals(1, controller.displayedBundles.size)
            assertEquals("v2-trash", controller.displayedBundles[0].bundleId)
        } finally {
            controller.dispose()
            service.dispose()
        }
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

        val studio = ArbStudioController(
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

        studio.renderState(state, image = null)

        assertTrue(studio.visibleHonestyChipLabels.isEmpty(), "Healthy bundle has 0 honesty chips")
        assertTrue(studio.exportButtonLabel.startsWith("選択範囲を書き出し ("), "Sub-range CTA shows seconds duration")
        assertTrue(studio.isSelectTapRangeVisible)
        assertTrue(studio.isSelectAroundCrashVisible)
        assertEquals(1, studio.viewerState?.screens?.size)
        assertEquals(1, studio.viewerState?.crashes?.size)
        assertEquals(1, studio.activeTapOverlays.size)
        assertTrue(studio.isExportCompletionBarVisible)

        val rows = studio.syncedRows
        val logRowIdx = rows.indexOfFirst { it.seq == 2L }
        assertTrue(logRowIdx >= 0)
        studio.triggerSnapInAtRow(logRowIdx)
        assertEquals(rows[logRowIdx].frameIndex, lastRange?.first)
        studio.triggerSourceJumpAtRow(logRowIdx)
        assertEquals(1, jumpedTargets.size)
        assertEquals("Checkout.kt:84", jumpedTargets[0].displayLabel)

        studio.renderState(state.copy(isLegacy = true, format = BundleFormat.LEGACY_SCHEMA4), image = null)
        assertFalse(studio.isTrashOriginalEnabled)
        assertFalse(studio.isTrashOriginalSelected)
        assertTrue(studio.visibleHonestyChipLabels.contains(LEGACY_READ_ONLY_BADGE_TEXT))
    }

    @Test
    fun allComposeUiSourcesContainZeroEmojiAndZeroHardcodedDarkSurfaceHex() {
        val repoRoot = System.getProperty("arb.repo.root")?.let { Paths.get(it) }
            ?: Paths.get("").toAbsolutePath().normalize().parent
        val srcDir = repoRoot.resolve("ide-plugin/src/main")
        assertTrue(Files.isDirectory(srcDir))
        Files.walk(srcDir).use { stream ->
            val files = stream.filter { Files.isRegularFile(it) }.toList()
            assertTrue(files.isNotEmpty())
            for (file in files) {
                val content = Files.readString(file)
                assertFalse(
                    ArbFormatters.containsEmoji(content),
                    "UI source ${file.fileName} must contain zero emoji",
                )
                assertFalse(
                    content.contains("0x1E, 0x1F, 0x22") || content.contains("#1E1F22", ignoreCase = true),
                    "UI source ${file.fileName} must not hardcode dark surface hex #1E1F22",
                )
                assertFalse(
                    content.contains("LineBorder"),
                    "UI source ${file.fileName} must not use boxy Swing LineBorder",
                )
            }
        }
    }
}

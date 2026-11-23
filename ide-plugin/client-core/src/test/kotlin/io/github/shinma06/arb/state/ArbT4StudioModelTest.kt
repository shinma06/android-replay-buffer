package io.github.shinma06.arb.state

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
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.model.StreamStatus
import io.github.shinma06.arb.model.StreamsSummary
import io.github.shinma06.arb.model.SystemStatusResponse
import io.github.shinma06.arb.model.TapRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.TimestampSemantics
import io.github.shinma06.arb.model.UnanchoredSummary
import io.github.shinma06.arb.model.VideoStreamMeta
import org.junit.jupiter.api.Test
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Comprehensive headless unit tests for T-4 UI ViewModels, formatters, event correlation,
 * deterministic presets, Legacy RO protections, SourceJumpParser, and the zero-emoji guard.
 */
class ArbT4StudioModelTest {
    private fun healthySummary(isLegacy: Boolean = false): TimelineSummaryResponse = TimelineSummaryResponse(
        bundleId = "b-healthy",
        path = "/tmp/b-healthy",
        format = if (isLegacy) BundleFormat.LEGACY_SCHEMA4 else BundleFormat.V2,
        isLegacy = isLegacy,
        createdAtUtc = "2026-11-23T10:00:00Z",
        provenance = Provenance.Original,
        streams = StreamsSummary(
            video = VideoStreamMeta(StreamStatus.PRESENT, 100L, false, TimestampSemantics.VIDEO_ENCODER_INPUT_ESTIMATE),
            audio = AudioStreamMeta(StreamStatus.PRESENT, AvOffsetStatus.CALIBRATED, 0L),
            logcat = LogcatStreamMeta(StreamStatus.PRESENT, LogCompleteness.VERIFIED_NO_OVERFLOW, 0L),
        ),
        unanchoredSummary = UnanchoredSummary(0L, 0L),
        epochs = listOf(EpochSpan("boot-1", 10_000_000_000L, 20_000_000_000L)), // 10 seconds
        frameCount = 100,
        keyframeCount = 5,
        logMainCount = 3,
        logUnanchoredCount = 0,
        tapMainCount = 1,
        tapUnanchoredCount = 0,
        discontinuityCount = 0,
    )

    @Test
    fun zeroEmojiGuardAuditsAllUiCopyAndSourceFiles() {
        val sampleStrings = listOf(
            APPROXIMATE_PREVIEW_BADGE_TEXT,
            LEGACY_READ_ONLY_BADGE_TEXT,
            UNANCHORED_TAB_TITLE,
            ArbFormatters.EXPORT_FULL_LABEL,
            ArbFormatters.SECONDARY_SCREENSHOT_LABEL,
            ArbFormatters.SECONDARY_LOGS_ONLY_LABEL,
            ArbFormatters.SECONDARY_START_CAPTURE_LABEL,
            ArbFormatters.SECONDARY_STOP_CAPTURE_LABEL,
            ArbFormatters.PRESET_TAP_RANGE_LABEL,
            ArbFormatters.PRESET_CRASH_RANGE_LABEL,
            ArbFormatters.PRESET_RESET_RANGE_LABEL,
            ArbFormatters.TRASH_ORIGINAL_TOGGLE_LABEL,
            ArbFormatters.EXPORT_TOAST_TITLE,
            ArbFormatters.formatPrimarySaveLabel(300, IdeDefaultSaveAction.SAVE_AND_OPEN),
            ArbFormatters.formatPrimarySaveLabel(45, IdeDefaultSaveAction.SAVE_ONLY),
            ArbFormatters.formatSecondarySaveLabel(IdeDefaultSaveAction.SAVE_AND_OPEN),
            ArbFormatters.formatRingMeter(272L, 300, 420L * 1024L * 1024L),
            ArbFormatters.formatExportCtaLabel(10L, 50L, 100L, 15_700L),
        ) + BundleFilter.entries.map { it.label } +
            EventStreamFilter.entries.map { it.label } +
            DiscontinuityKind.entries.map { DiscontinuityFormatter.label(it) }

        for (text in sampleStrings) {
            assertFalse(ArbFormatters.containsEmoji(text), "UI string must not contain emoji: '$text'")
            assertFalse(text.contains("エビデンス"), "UI string must stay generic (never 'エビデンス'): '$text'")
        }

        // Scan ALL main Kotlin source files under ide-plugin/ to guarantee zero emoji chrome!
        val repoRoot = System.getProperty("arb.repo.root")?.let(Paths::get)
            ?: Paths.get("").toAbsolutePath().normalize().parent
        val idePluginDir = repoRoot.resolve("ide-plugin")
        if (Files.isDirectory(idePluginDir)) {
            val kotlinSources = listOf(
                idePluginDir.resolve("src/main/kotlin"),
                idePluginDir.resolve("client-core/src/main/kotlin"),
            ).filter { Files.isDirectory(it) }
                .flatMap { dir ->
                    Files.walk(dir).use { stream ->
                        stream.filter { it.toString().endsWith(".kt") }.toList()
                    }
                }
            assertTrue(kotlinSources.isNotEmpty(), "Expected Kotlin main source files under ide-plugin/")
            for (file in kotlinSources) {
                val content = Files.readString(file)
                assertFalse(
                    ArbFormatters.containsEmoji(content),
                    "Main source file ${file.fileName} contains forbidden emoji codepoints!",
                )
            }
        }
    }

    @Test
    fun strictSecondsNotationNeverConvertsToMinutes() {
        assertEquals("300s", ArbFormatters.formatSeconds(300))
        assertEquals("45s", ArbFormatters.formatSeconds(45))
        assertEquals("直前 300s を保存して開く", ArbFormatters.formatPrimarySaveLabel(300, IdeDefaultSaveAction.SAVE_AND_OPEN))
        assertEquals("直前 45s を保存して開く", ArbFormatters.formatPrimarySaveLabel(45, IdeDefaultSaveAction.SAVE_AND_OPEN))
        assertFalse(ArbFormatters.formatPrimarySaveLabel(300, IdeDefaultSaveAction.SAVE_AND_OPEN).contains("分"))

        val meter = ArbFormatters.formatRingMeter(272L, 300, 420L * 1024L * 1024L)
        assertEquals("272s / 300s · 420 MB", meter)
        assertFalse(meter.contains("分"))
    }

    @Test
    fun dualSaveActionSwapsPrimaryAndSecondaryLabelsCleanly() {
        val saveAndOpen = CaptureBarState(config = ArbConfig(saveWindowSecs = 300, ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_AND_OPEN))
        assertEquals("直前 300s を保存して開く", saveAndOpen.primarySaveLabel)
        assertEquals("保存のみ", saveAndOpen.secondarySaveLabel)

        val saveOnly = CaptureBarState(config = ArbConfig(saveWindowSecs = 120, ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY))
        assertEquals("直前 120s を保存のみ", saveOnly.primarySaveLabel)
        assertEquals("保存して開く", saveOnly.secondarySaveLabel)
    }

    @Test
    fun tier2BannersAndHonestyChipsStayHiddenWhenHealthyAndAppearOnIssues() {
        val healthyStatus = SystemStatusResponse(
            pid = 1L,
            uptimeMs = 1000L,
            capture = CaptureStatusView(
                active = true,
                connected = true,
                serial = "pixel-9",
                storagePressureStopped = false,
                videoFrames = 300L,
                audioFrames = 100L,
                logRecords = 50L,
                tapRecords = 2L,
                discontinuities = 0L,
                staticHoldEvents = 0L,
            ),
            ringChunkCount = 4L,
            ringTotalBytes = 10_000_000L,
            ringPinnedChunkCount = 0L,
            activeViewports = 0L,
            activeSaveJobs = 0L,
            recovery = RecoverySummaryView(),
        )
        val healthyBar = CaptureBarReducer.fromStatus(CaptureBarState(), healthyStatus)
        assertFalse(healthyBar.hasVisibleBanners, "Healthy capture must show zero Tier-2 banners")
        assertEquals("REC", healthyBar.statusBadgeText)

        val pressureBar = CaptureBarReducer.fromStatus(
            CaptureBarState(),
            healthyStatus.copy(capture = healthyStatus.capture.copy(active = false, storagePressureStopped = true)),
        )
        assertTrue(pressureBar.hasVisibleBanners)
        assertEquals(listOf(CaptureBannerKind.STORAGE_PRESSURE_STOP), pressureBar.banners.map { it.kind })

        // Healthy studio bundle -> 0 honesty chips
        val healthyViewer = TimelineViewerState(
            bundleId = "b-healthy",
            path = "/tmp/b-healthy",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "l1",
            summary = healthySummary(),
            frameCount = 100L,
        )
        assertTrue(healthyViewer.tier2HonestyChips.isEmpty(), "Healthy bundle must render 0 Tier-2 honesty chips")

        // Degraded + Legacy bundle -> shows relevant Tier-2 honesty chips
        val degradedSummary = healthySummary(isLegacy = true).copy(
            streams = StreamsSummary(
                audio = AudioStreamMeta(StreamStatus.PRESENT, AvOffsetStatus.CALIBRATED, 106L),
                logcat = LogcatStreamMeta(StreamStatus.PRESENT, LogCompleteness.UNVERIFIED, 0L),
            ),
            discontinuityCount = 2,
        )
        val degradedViewer = healthyViewer.copy(
            isLegacy = true,
            summary = degradedSummary,
            discontinuities = listOf(
                DiscontinuityEvent(DiscontinuityKind.DEVICE_SLEEP, pcContStartNs = 1L),
                DiscontinuityEvent(DiscontinuityKind.STREAM_GAP, pcContStartNs = 2L),
            ),
        )
        val chipLabels = degradedViewer.tier2HonestyChips.map { it.label }
        assertTrue(chipLabels.any { it.contains("±106ms") })
        assertTrue(chipLabels.any { it.contains("UNVERIFIED") })
        assertTrue(chipLabels.any { it.contains("ギャップ 2 件") })
        assertTrue(chipLabels.contains(LEGACY_READ_ONLY_BADGE_TEXT))
    }

    @Test
    fun trimRangeSyncTapLogCorrelation300msAndHonestTapDisplayWork() {
        val base = 10_000_000_000L // frame 0
        val tapAt3s = TapRecord(
            seq = 1L,
            pcIngestContNs = base + 3_000_000_000L,
            alignment = TimeAlignment.Anchored("boot-1", base + 3_000_000_000L, 1_000_000L, TimestampSemantics.INPUT_EVENT_TIME),
            bucket = RecordBucket.MAIN,
            action = "DOWN",
            x = 540.4,
            y = 1280.6,
        )
        val correlatedLogAt3150ms = LogRecord(
            seq = 10L,
            pcIngestContNs = base + 3_150_000_000L,
            alignment = TimeAlignment.Anchored("boot-1", base + 3_150_000_000L, 1_000_000L, TimestampSemantics.LOGCAT_DEVICE_READ_BOOTTIME),
            bucket = RecordBucket.MAIN,
            level = "I",
            tag = "Checkout",
            message = "Clicked submit at com.example.shop.CheckoutViewModel.submit(CheckoutViewModel.kt:84)",
        )
        val laterLogAt8sOutsideTrim = LogRecord(
            seq = 11L,
            pcIngestContNs = base + 8_000_000_000L,
            alignment = TimeAlignment.Anchored("boot-1", base + 8_000_000_000L, 1_000_000L, TimestampSemantics.LOGCAT_DEVICE_READ_BOOTTIME),
            bucket = RecordBucket.MAIN,
            level = "D",
            tag = "Other",
            message = "Unrelated background tick",
        )

        val state = TimelineViewerState(
            bundleId = "b-sync",
            path = "/tmp/b-sync",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "l1",
            summary = healthySummary(),
            frameCount = 100L,
            inFrameIndex = 20L,
            outFrameIndex = 60L,
            currentFrameIndex = 30L,
            mainTaps = listOf(tapAt3s),
            mainLogs = listOf(correlatedLogAt3150ms, laterLogAt8sOutsideTrim),
        )

        val rows = state.buildSyncedEventRows()
        val tapRow = rows.first { it.kind == SyncedEventKind.TAP }
        assertEquals("Tap #1 (540, 1281)", tapRow.title, "Tap display must stay honest coordinates only")
        assertTrue(tapRow.isInTrimRange)

        val corrRow = rows.first { it.seq == 10L }
        assertEquals(1, corrRow.correlatedTapIndex, "Log within 0..300ms after Tap #1 must correlate to Tap #1")
        assertTrue(corrRow.isInTrimRange, "Log at 3.15s (~frame 31) is inside [20..60]")
        assertEquals("CheckoutViewModel.kt:84", corrRow.sourceJumpTarget?.displayLabel)

        val outsideRow = rows.first { it.seq == 11L }
        assertNull(outsideRow.correlatedTapIndex, "Log 5s after tap must NOT correlate")
        assertFalse(outsideRow.isInTrimRange, "Log at 8s (~frame 79) must dim outside [20..60]")

        // Tap ripple overlay within ±250ms of frame 30 (3.03s) includes Tap #1
        assertEquals(1, state.activeTapsAtFrame(30L).size)
        // Far frame (80) has 0 active tap overlays
        assertEquals(0, state.activeTapsAtFrame(80L).size)

        // Preset availability strictly reflects recorded data
        assertTrue(state.canSelectTapRange)
        assertFalse(state.canSelectAroundCrash)
    }

    @Test
    fun sourceJumpParserSkipsFrameworkFramesAndPrefersAppFrames() {
        val target = SourceJumpParser.parseLogText(
            "at android.os.Handler.dispatchMessage(Handler.java:106)\n" +
                "at java.lang.Thread.run(Thread.java:1012)\n" +
                "at com.example.app.MainActivity.onCreate(MainActivity.kt:12)",
        )
        assertNotNull(target)
        assertEquals("com.example.app.MainActivity", target.classFqn)
        assertEquals(12, target.lineNumber)
        val fqn = SourceJumpParser.parseLogText("java.lang.IllegalStateException wrapped com.example.app.Foo failed")
        assertNotNull(fqn)
        assertEquals("com.example.app.Foo", fqn.classFqn)
    }

    @Test
    fun boottimeMappingUsesFilmstripAcrossEpochs() {
        val base = healthySummary()
        val summary = base.copy(
            epochs = listOf(
                io.github.shinma06.arb.model.EpochSpan("a", 1_000_000_000L, 11_000_000_000L),
                io.github.shinma06.arb.model.EpochSpan("b", 900_000_000_000L, 910_000_000_000L),
            ),
        )
        val state = TimelineViewerState(
            bundleId = "b-multi", path = "/tmp/b-multi", format = BundleFormat.V2, isLegacy = false,
            leaseId = "l", summary = summary, frameCount = 101L,
            filmstrip = listOf(
                io.github.shinma06.arb.model.FilmstripEntryMeta(0L, 1_000_000_000L, 1, 1),
                io.github.shinma06.arb.model.FilmstripEntryMeta(50L, 11_000_000_000L, 1, 1),
                io.github.shinma06.arb.model.FilmstripEntryMeta(51L, 900_000_000_000L, 1, 1),
                io.github.shinma06.arb.model.FilmstripEntryMeta(100L, 910_000_000_000L, 1, 1),
            ),
        )
        assertEquals(25L, state.boottimeNsToFrameIndex(6_000_000_000L))
        assertTrue(state.boottimeNsToFrameIndex(905_000_000_000L) in 74L..76L)
        assertEquals(100L, state.boottimeNsToFrameIndex(2_000_000_000_000L))
    }

    @Test
    fun sourceJumpParserHandlesStackTracesFileLinesActivitiesAndCrashes() {
        val stack = SourceJumpParser.parseLogText(
            "    at com.example.app.ui.CheckoutViewModel\$submit\$1.invokeSuspend(CheckoutViewModel.kt:84)",
        )
        assertNotNull(stack)
        assertEquals("com.example.app.ui.CheckoutViewModel", stack.classFqn)
        assertEquals("CheckoutViewModel.kt", stack.fileName)
        assertEquals(84, stack.lineNumber)
        assertEquals("CheckoutViewModel.kt:84", stack.displayLabel)

        val inline = SourceJumpParser.parseLogText("Error raised near (LoginActivity.kt:42)")
        assertNotNull(inline)
        assertEquals("LoginActivity.kt", inline.fileName)
        assertEquals(42, inline.lineNumber)

        val screenTarget = SourceJumpParser.fromScreenSpan(
            ScreenSpan(
                startNs = 1L,
                endNs = 2L,
                startFrameIndex = 0L,
                endFrameIndex = 10L,
                packageName = "com.example.app",
                activityName = "com.example.app.ui.SettingsActivity",
                shortName = "SettingsActivity",
            ),
        )
        assertEquals("com.example.app.ui.SettingsActivity", screenTarget.classFqn)
        assertEquals("SettingsActivity", screenTarget.shortClassName)

        val crashTarget = SourceJumpParser.fromCrashMarker(
            CrashMarker(
                boottimeNs = 10L,
                frameIndexHint = 5L,
                processName = "com.example.app",
                packageName = "com.example.app",
                pid = 100,
                exceptionClass = "com.example.app.DomainCrashException",
                message = "Failed in PaymentActivity.kt:199",
                sourceLogSeq = 12L,
            ),
        )
        assertEquals("PaymentActivity.kt", crashTarget.fileName)
        assertEquals(199, crashTarget.lineNumber)
    }

    @Test
    fun legacyReadOnlyBlocksTrashOriginalAndCatalogFiltersWork() {
        val legacyState = TimelineViewerState(
            bundleId = "leg-1",
            path = "/old/leg-1",
            format = BundleFormat.LEGACY_SCHEMA4,
            isLegacy = true,
            leaseId = "l-leg",
            summary = healthySummary(isLegacy = true),
            frameCount = 50L,
        )
        assertFalse(legacyState.canTrashOriginalAfterExport)
        val attemptedEnable = TimelineViewerReducer.setTrashOriginalAfterExport(legacyState, true)
        assertFalse(attemptedEnable.effectiveTrashOriginalAfterExport, "Legacy RO must NEVER enable trash_original")

        val v2State = legacyState.copy(isLegacy = false, format = BundleFormat.V2)
        assertTrue(v2State.canTrashOriginalAfterExport)
        assertTrue(TimelineViewerReducer.setTrashOriginalAfterExport(v2State, true).effectiveTrashOriginalAfterExport)

        val catalog = BundleCatalogState.fromEntries(
            entries = listOf(
                BundleEntryView("orig-1", "/a", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, null, "2026-11-23T00:00:00Z", 0),
                BundleEntryView("trim-1", "/b", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, "orig-1", "2026-11-23T00:01:00Z", 0),
                BundleEntryView("crash-1", "/c", CatalogFormatVersion.V2, BundleLifecycleState.ACTIVE, null, null, "2026-11-23T00:02:00Z", 0),
                BundleEntryView("trash-1", "/d", CatalogFormatVersion.V2, BundleLifecycleState.TRASHED, "2026-11-23T00:03:00Z", null, "2026-11-23T00:00:30Z", 0),
            ),
            crashBundleIds = setOf("crash-1"),
        )
        assertEquals(3, catalog.copy(activeFilter = BundleFilter.ALL).filteredItems.size)
        assertEquals(listOf("trim-1"), catalog.copy(activeFilter = BundleFilter.TRIMMED).filteredItems.map { it.bundleId })
        assertEquals(listOf("crash-1"), catalog.copy(activeFilter = BundleFilter.CRASH).filteredItems.map { it.bundleId })
        assertEquals(listOf("trash-1"), catalog.copy(activeFilter = BundleFilter.TRASH).filteredItems.map { it.bundleId })
    }
}

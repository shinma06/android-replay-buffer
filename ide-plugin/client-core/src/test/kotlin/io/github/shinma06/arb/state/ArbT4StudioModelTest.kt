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
import io.github.shinma06.arb.model.GestureKind
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
    fun t6hSwipeRowsShowDirectionAndFlowThroughToTheVideoOverlay() {
        val base = 10_000_000_000L
        val swipeUp = TapRecord(
            seq = 1L,
            pcIngestContNs = base + 1_000_000_000L,
            alignment = TimeAlignment.Anchored("boot-1", base + 1_000_000_000L, 1_000_000L, TimestampSemantics.INPUT_EVENT_TIME),
            bucket = RecordBucket.MAIN,
            action = "SWIPE",
            x = 540.0,
            y = 1800.0,
            gestureKind = GestureKind.SWIPE,
            endX = 540.0,
            endY = 400.0,
            endTOffsetNs = 150_000_000L,
        )
        val plainTap = TapRecord(
            seq = 2L,
            pcIngestContNs = base + 2_000_000_000L,
            alignment = TimeAlignment.Anchored("boot-1", base + 2_000_000_000L, 1_000_000L, TimestampSemantics.INPUT_EVENT_TIME),
            bucket = RecordBucket.MAIN,
            action = "TAP",
            x = 100.0,
            y = 200.0,
        )

        val state = TimelineViewerState(
            bundleId = "b-swipe",
            path = "/tmp/b-swipe",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "l1",
            summary = healthySummary(),
            frameCount = 100L,
            currentFrameIndex = 10L,
            mainTaps = listOf(swipeUp, plainTap),
        )

        val rows = state.buildSyncedEventRows()
        val swipeRow = rows.first { it.seq == 1L }
        assertEquals("Swipe #1 (540, 1800) -> (540, 400) (上スワイプ)", swipeRow.title)
        val tapRow = rows.first { it.seq == 2L }
        assertEquals("Tap #2 (100, 200)", tapRow.title, "a plain Tap row must never show an arrow or direction")

        // The video canvas overlay (`ActiveTapOverlay`) must carry the gesture kind and end point
        // through so the Viewer can render a polyline instead of a ripple for the Swipe.
        // swipeUp is anchored at base+1s == frame 10; plainTap is anchored at base+2s == frame 20
        // (100ms/frame over the 10s/100-frame epoch), each queried at its own exact frame.
        val swipeOverlay = state.activeTapsAtFrame(10L).first { it.tapIndex1Based == 1 }
        assertEquals(GestureKind.SWIPE, swipeOverlay.gestureKind)
        assertEquals(540.0, swipeOverlay.endX)
        assertEquals(400.0, swipeOverlay.endY)
        val tapOverlay = state.activeTapsAtFrame(20L).first { it.tapIndex1Based == 2 }
        assertEquals(GestureKind.TAP, tapOverlay.gestureKind)
        assertNull(tapOverlay.endX)

        // A long swipe (>250ms duration) must stay visible across its full stroke interval
        // [tapBt, tapBt + endTOffsetNs] instead of vanishing 250ms after DOWN.
        val longSwipe = swipeUp.copy(seq = 3L, endTOffsetNs = 600_000_000L) // 1.0s..1.6s => frames 10..16
        val longState = state.copy(mainTaps = listOf(longSwipe))
        val midStroke = longState.activeTapsAtFrame(14L) // 1.4s = 400ms after DOWN, still inside stroke
        assertEquals(1, midStroke.size, "long swipe must remain visible mid-stroke")
        assertEquals(0L, midStroke.first().deltaMs)
        assertTrue(longState.activeTapsAtFrame(20L).isEmpty(), "long swipe must disappear >250ms after end")
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
        assertEquals(null, SourceJumpParser.parseLogText("at android.os.Looper.loop(Looper.java:223)"))
        val fqn = SourceJumpParser.parseLogText("java.lang.IllegalStateException wrapped com.example.app.Foo failed")
        assertNotNull(fqn)
        assertEquals("com.example.app.Foo", fqn.classFqn)
    }

    @Test
    fun boottimeMappingUsesBootAnchorsAcrossEpochs() {
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
        assertNull(state.boottimeNsToFrameIndex(2_000_000_000_000L), "unknown epoch stays unsynchronized")
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

    @Test
    fun screenSpansCrossingTheRecordingEdgesAreClippedNotHidden() {
        val s = 1_000_000_000L
        val range = io.github.shinma06.arb.model.EpochFrameRange(0, "boot-0", 0L, 9L, 100 * s, 101 * s)
        fun state(withRanges: Boolean) = TimelineViewerState(
            bundleId = "b-clip",
            path = "/tmp/b-clip",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "l-clip",
            summary = healthySummary().copy(
                epochs = listOf(EpochSpan("boot-0", 100 * s, 101 * s)),
                epochFrameRanges = if (withRanges) listOf(range) else emptyList(),
            ),
            frameCount = 10L,
            axis = io.github.shinma06.arb.playback.TimelineCoordinateAxis(10, List(10) { it * s / 10 }, lastDurationNs = s / 10),
        )
        fun span(start: Long, end: Long) = ScreenSpan(start, end, packageName = "p", activityName = "p.A", shortName = "A")
        for (withRanges in listOf(true, false)) {
            val st = state(withRanges)
            // Already in the foreground when capture started: starts at media 0.
            assertEquals(0L, st.screenTime(span(99 * s, 100 * s + s / 2)))
            assertEquals(s / 2, st.screenTime(span(99 * s, 100 * s + s / 2), end = true))
            // Still in the foreground at the end: ends at the exclusive media end.
            assertEquals(s / 2, st.screenTime(span(100 * s + s / 2, 105 * s)))
            assertEquals(s, st.screenTime(span(100 * s + s / 2, 105 * s), end = true))
            // Entirely outside the recording, and pre-roll point events, stay unmapped.
            assertNull(st.screenTime(span(90 * s, 95 * s)))
            assertNull(st.eventTime(99 * s, "boot-0"))
        }
    }

    @Test
    fun multiEpochFrameRangesMapLogsTapsCrashesWithoutCrossEpochBleed() {
        val base = healthySummary()
        // Epoch 0: frames 0..50 spanning boottime 100s..110s
        // Epoch 1 (post-reboot! boottime resets back to 5s..15s): frames 51..100
        val epoch0 = io.github.shinma06.arb.model.EpochFrameRange(
            epochIndex = 0,
            bootId = "boot-0",
            startFrameIndex = 0L,
            endFrameIndex = 50L,
            startBoottimeNs = 100_000_000_000L,
            endBoottimeNs = 110_000_000_000L,
        )
        val epoch1 = io.github.shinma06.arb.model.EpochFrameRange(
            epochIndex = 1,
            bootId = "boot-1",
            startFrameIndex = 51L,
            endFrameIndex = 100L,
            startBoottimeNs = 5_000_000_000L,
            endBoottimeNs = 15_000_000_000L,
        )
        val summary = base.copy(
            epochs = listOf(
                EpochSpan("boot-0", 100_000_000_000L, 110_000_000_000L),
                EpochSpan("boot-1", 5_000_000_000L, 15_000_000_000L),
            ),
            epochFrameRanges = listOf(epoch0, epoch1),
            frameCount = 101,
        )

        val tapE0 = TapRecord(
            seq = 1L,
            bootId = "boot-0",
            pcIngestContNs = 1L,
            alignment = TimeAlignment.Anchored("boot-0", 105_000_000_000L, 1_000_000L, TimestampSemantics.INPUT_EVENT_TIME),
            bucket = RecordBucket.MAIN,
            action = "DOWN",
            x = 540.0,
            y = 1200.0,
        )
        val tapE1 = TapRecord(
            seq = 2L,
            bootId = "boot-1",
            pcIngestContNs = 2L,
            alignment = TimeAlignment.Anchored("boot-1", 10_000_000_000L, 1_000_000L, TimestampSemantics.INPUT_EVENT_TIME),
            bucket = RecordBucket.MAIN,
            action = "DOWN",
            x = 200.0,
            y = 400.0,
        )
        val logE0 = LogRecord(
            seq = 10L,
            bootId = "boot-0",
            pcIngestContNs = 3L,
            alignment = TimeAlignment.Anchored("boot-0", 105_100_000_000L, 1_000_000L, TimestampSemantics.LOGCAT_DEVICE_READ_BOOTTIME),
            bucket = RecordBucket.MAIN,
            level = "I",
            tag = "Epoch0",
            message = "right after tap 1 in epoch 0",
        )
        val logE1 = LogRecord(
            seq = 11L,
            bootId = "boot-1",
            pcIngestContNs = 4L,
            alignment = TimeAlignment.Anchored("boot-1", 10_100_000_000L, 1_000_000L, TimestampSemantics.LOGCAT_DEVICE_READ_BOOTTIME),
            bucket = RecordBucket.MAIN,
            level = "E",
            tag = "Epoch1",
            message = "right after tap 2 in epoch 1",
        )
        val crashE1 = CrashMarker(
            boottimeNs = 10_000_000_000L,
            frameIndexHint = null, // Force epoch lookup via sourceLogSeq -> boot-1
            processName = "com.app",
            packageName = "com.app",
            pid = 42,
            exceptionClass = "java.lang.IllegalStateException",
            message = "post-reboot crash",
            sourceLogSeq = 11L,
        )

        val state = TimelineViewerState(
            bundleId = "b-reboot",
            path = "/tmp/b-reboot",
            format = BundleFormat.V2,
            isLegacy = false,
            leaseId = "l-r",
            summary = summary,
            frameCount = 101L,
            crashes = listOf(crashE1),
            mainTaps = listOf(tapE0, tapE1),
            mainLogs = listOf(logE0, logE1),
        )

        // Exact per-epoch frame mapping (midpoint of epoch 0 = 25; midpoint of epoch 1 = 76)
        assertEquals(25L, state.boottimeNsToFrameIndex(105_000_000_000L, "boot-0"))
        assertTrue(state.boottimeNsToFrameIndex(10_000_000_000L, "boot-1") in 75L..76L)
        // Unknown epoch must return null (zero cross-epoch bleed!)
        assertNull(state.boottimeNsToFrameIndex(105_000_000_000L, "boot-unknown"))
        // Overlapping epoch ranges without a boot id are ambiguous: never guess into one epoch.
        val overlapping = state.copy(
            summary = summary.copy(
                epochFrameRanges = summary.epochFrameRanges.map {
                    it.copy(startBoottimeNs = 0L, endBoottimeNs = 200_000_000_000L)
                },
            ),
        )
        assertNull(overlapping.boottimeNsToFrameIndex(10_000_000_000L))

        // Total duration sums both epochs cleanly (10s + 10s = 20s) even though epoch 1 reset boottime!
        assertEquals(20_200L, state.totalDurationMs, "duration includes the inferred final sample")
        assertTrue(state.primaryCrashFrameIndex() in 75L..76L)

        // Tap overlay at epoch-0 frame 25 shows ONLY Tap #1; at epoch-1 frame 76 shows ONLY Tap #2.
        assertEquals(listOf(1), state.activeTapsAtFrame(25L).map { it.tapIndex1Based })
        assertEquals(listOf(2), state.activeTapsAtFrame(76L).map { it.tapIndex1Based })

        // Synced event rows keep epoch 0 strictly before epoch 1 and correlate only within same epoch.
        val rows = state.buildSyncedEventRows()
        val rowLog0 = rows.first { it.kind == SyncedEventKind.LOG && it.seq == 10L }
        val rowLog1 = rows.first { it.kind == SyncedEventKind.LOG && it.seq == 11L }
        assertEquals(1, rowLog0.correlatedTapIndex)
        assertEquals(2, rowLog1.correlatedTapIndex)
        assertTrue(rows.indexOf(rowLog0) < rows.indexOf(rowLog1), "epoch 0 events must sort before epoch 1")
    }

    @Test
    fun honestBufferedSecsFromSystemStatusOverridesFrameEstimateAndLabelsFilling() {
        val status = SystemStatusResponse(
            pid = 1L,
            uptimeMs = 8_000L,
            capture = CaptureStatusView(
                active = true,
                connected = true,
                serial = "emulator-5554",
                storagePressureStopped = false,
                videoFrames = 2L, // static screen -> frame estimate would falsely say 0s!
                audioFrames = 0L,
                logRecords = 20L,
                tapRecords = 0L,
                discontinuities = 0L,
                staticHoldEvents = 0L,
            ),
            ringChunkCount = 2L,
            ringTotalBytes = 2L * 1024L * 1024L,
            ringPinnedChunkCount = 0L,
            bufferedSecs = 8L,
            activeViewports = 0L,
            activeSaveJobs = 0L,
            recovery = RecoverySummaryView(),
        )
        val filling = CaptureBarReducer.fromStatus(CaptureBarState(), status)
        assertEquals(8L, filling.bufferedWindowSecs)
        assertEquals("8s / 300s · 2.0 MB (蓄積中)", filling.ringMeterText)
        assertFalse(ArbFormatters.containsEmoji(filling.ringMeterText))

        val full = CaptureBarReducer.fromStatus(CaptureBarState(), status.copy(bufferedSecs = 350L))
        assertEquals(300L, full.bufferedWindowSecs, "must clamp to saveWindowSecs")
        assertEquals("300s / 300s · 2.0 MB", full.ringMeterText)
    }
}

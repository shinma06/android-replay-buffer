package io.github.shinma06.arb.ide

import com.intellij.icons.AllIcons
import com.intellij.notification.NotificationType
import com.intellij.openapi.project.DumbAwareToggleAction
import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.StreamsSummary
import io.github.shinma06.arb.model.TimelineSummaryResponse
import io.github.shinma06.arb.model.UnanchoredSummary
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.TrimCloseState
import io.github.shinma06.arb.ui.ArbStudioController
import io.github.shinma06.arb.ui.StudioExportFormat
import org.junit.jupiter.api.Test
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ArbT6iRunningDevicesAndBurnTapsTest {
    private fun repoRoot(): Path {
        System.getProperty("arb.repo.root")?.let { raw ->
            val p = Paths.get(raw).toAbsolutePath().normalize()
            if (Files.isDirectory(p.resolve("core")) && Files.isDirectory(p.resolve("ide-plugin"))) {
                return p
            }
        }
        var cur = Paths.get("").toAbsolutePath().normalize()
        repeat(8) {
            if (Files.isDirectory(cur.resolve("core")) && Files.isDirectory(cur.resolve("ide-plugin"))) {
                return cur
            }
            cur = cur.parent ?: return cur
        }
        error("Could not locate repository root")
    }

    @Test
    fun pluginXmlRegistersRunningDevicesGroupOnStreamingToolbarSecondaryWithExplicitIcons() {
        val pluginXml = Files.readString(repoRoot().resolve("ide-plugin/src/main/resources/META-INF/plugin.xml"))

        // Verified in Android Studio 2026.2.2.8 android.jar (META-INF/streaming.xml + AbstractDevicePanel):
        // StreamingToolbarSecondary is rendered on BOTH Embedded Emulator and Physical Device Mirroring panels.
        val secondaryAttachments = Regex("""group-id="StreamingToolbarSecondary"""").findAll(pluginXml).count()
        assertEquals(1, secondaryAttachments, "StreamingToolbarSecondary must be attached exactly once (zero duplicates)")
        assertTrue(pluginXml.contains("""<group id="io.github.shinma06.arb.RunningDevices">"""))
        assertTrue(pluginXml.contains("""id="io.github.shinma06.arb.RunningDevices.ToggleCapture""""))
        assertTrue(pluginXml.contains("""id="io.github.shinma06.arb.RunningDevices.Save""""))
        assertTrue(pluginXml.contains("""id="io.github.shinma06.arb.RunningDevices.Screenshot""""))

        // Every <action ...> block in plugin.xml must declare an explicit icon="..." attribute.
        val actionBlocks = Regex("""<action\b[^>]*>""", setOf(RegexOption.DOT_MATCHES_ALL)).findAll(pluginXml).toList()
        assertTrue(actionBlocks.size >= 9, "Expected all ARB actions in plugin.xml, got ${actionBlocks.size}")
        for (match in actionBlocks) {
            val tag = match.value
            assertTrue(
                tag.contains("""icon="AllIcons."""),
                "Every <action> in plugin.xml must declare an explicit AllIcons icon: $tag",
            )
        }
        assertFalse(ArbFormatters.containsEmoji(pluginXml), "plugin.xml must contain zero emoji")
    }

    @Test
    fun toggleCaptureActionExtendsDumbAwareToggleAction() {
        val action: Any = ArbToggleCaptureAction()
        assertTrue(
            action is DumbAwareToggleAction,
            "ArbToggleCaptureAction must extend DumbAwareToggleAction for pressed/toggled toolbar rendering",
        )
    }

    @Test
    fun toolbarPresentationStateReflectsHonestDaemonAndDeviceStates() {
        // 1. Cold start (not probed yet): enabled so user can click to lazily connect/spawn arbd.
        val unprobed = ArbToolbarDaemonSnapshot(probed = false)
        assertTrue(ArbToolbarPresentationState.toggleCapture(unprobed).enabled)
        assertFalse(ArbToolbarPresentationState.toggleCapture(unprobed).selected)
        assertTrue(ArbToolbarPresentationState.quickSave(unprobed, 300).enabled)
        assertTrue(ArbToolbarPresentationState.screenshot(unprobed).enabled)

        // 2. Connected, no online device: ToggleCapture & Screenshot disabled with clear tooltip; QuickSave disabled when no buffer.
        val noDevice = ArbToolbarDaemonSnapshot(
            probed = true,
            daemonConnected = true,
            captureActive = false,
            hasBufferedData = false,
            hasOnlineDevice = false,
            selectedDeviceSerial = null,
            disabledDeviceHint = "端末未検出（USB / エミュレータを接続）",
        )
        val toggleNoDev = ArbToolbarPresentationState.toggleCapture(noDevice)
        assertFalse(toggleNoDev.enabled)
        assertEquals(AllIcons.Actions.Restart, toggleNoDev.icon)
        assertTrue(toggleNoDev.description.contains("端末未検出"))

        val saveNoBuf = ArbToolbarPresentationState.quickSave(noDevice, 120)
        assertFalse(saveNoBuf.enabled)
        assertTrue(saveNoBuf.description.contains("先にキャプチャを開始"))
        assertTrue(saveNoBuf.text.contains("120s"))

        val shotNoDev = ArbToolbarPresentationState.screenshot(noDevice)
        assertFalse(shotNoDev.enabled)
        assertTrue(shotNoDev.description.contains("端末未検出"))

        // 3. Connected + online device + idle: ToggleCapture & Screenshot enabled.
        val onlineIdle = ArbToolbarDaemonSnapshot(
            probed = true,
            daemonConnected = true,
            captureActive = false,
            hasBufferedData = false,
            hasOnlineDevice = true,
            selectedDeviceSerial = "emulator-5554",
        )
        val toggleIdle = ArbToolbarPresentationState.toggleCapture(onlineIdle)
        assertTrue(toggleIdle.enabled)
        assertFalse(toggleIdle.selected)
        assertTrue(toggleIdle.description.contains("emulator-5554"))
        assertTrue(ArbToolbarPresentationState.screenshot(onlineIdle).enabled)

        // 4. Connected + active capture: ToggleCapture selected=true & Suspend icon, Save & Screenshot enabled.
        val active = onlineIdle.copy(captureActive = true, hasBufferedData = true)
        val toggleActive = ArbToolbarPresentationState.toggleCapture(active)
        assertTrue(toggleActive.enabled)
        assertTrue(toggleActive.selected)
        assertEquals(AllIcons.Actions.Suspend, toggleActive.icon)
        assertTrue(ArbToolbarPresentationState.quickSave(active, 300).enabled)
        assertTrue(ArbToolbarPresentationState.screenshot(active).enabled)
        assertTrue(ArbToolbarPresentationState.saveLogsOnly(active, 300).enabled)

        for (pres in listOf(toggleNoDev, saveNoBuf, shotNoDev, toggleIdle, toggleActive)) {
            assertFalse(ArbFormatters.containsEmoji(pres.text))
            assertFalse(ArbFormatters.containsEmoji(pres.description))
        }
    }

    @Test
    fun notificationsBuildExpectedRichActionsAndEscapeHtmlAcrossAllKinds() {
        val replayClosed = ArbNotifications.buildReplaySavedSpec(
            bundleId = "b-<1>&\"x\"",
            bundlePath = "/tmp/bundles/b-1",
            windowSecs = 60,
            openedInStudio = false,
        )
        assertEquals(NotificationType.INFORMATION, replayClosed.type)
        assertTrue(replayClosed.content.contains("b-&lt;1&gt;&amp;&quot;x&quot;"))
        assertEquals(
            listOf(
                ArbNotificationActionId.OPEN_VIEWER,
                ArbNotificationActionId.OPEN_FOLDER,
                ArbNotificationActionId.COPY_PATH,
                ArbNotificationActionId.MOVE_TO_TRASH,
            ),
            replayClosed.actions.map { it.id },
        )

        val replayOpened = ArbNotifications.buildReplaySavedSpec(
            bundleId = "b-2",
            bundlePath = "/tmp/bundles/b-2",
            windowSecs = 60,
            openedInStudio = true,
        )
        assertEquals(
            listOf(
                ArbNotificationActionId.OPEN_FOLDER,
                ArbNotificationActionId.COPY_PATH,
                ArbNotificationActionId.MOVE_TO_TRASH,
            ),
            replayOpened.actions.map { it.id },
        )

        val logsOnly = ArbNotifications.buildLogsOnlySavedSpec(
            bundleId = "logs-1",
            bundlePath = "/tmp/bundles/logs-1",
            windowSecs = 30,
        )
        assertEquals(
            listOf(
                ArbNotificationActionId.OPEN_VIEWER,
                ArbNotificationActionId.OPEN_FOLDER,
                ArbNotificationActionId.COPY_PATH,
                ArbNotificationActionId.MOVE_TO_TRASH,
            ),
            logsOnly.actions.map { it.id },
        )

        val imageSaved = ArbNotifications.buildImageSavedSpec(
            imagePath = "/tmp/shots/screen_<1>.png",
            width = 1080,
            height = 2400,
        )
        assertTrue(imageSaved.content.contains("screen_&lt;1&gt;.png"))
        assertEquals(
            listOf(
                ArbNotificationActionId.OPEN_IMAGE,
                ArbNotificationActionId.OPEN_FOLDER,
                ArbNotificationActionId.COPY_PATH,
                ArbNotificationActionId.MOVE_TO_TRASH,
            ),
            imageSaved.actions.map { it.id },
        )

        val exportDone = ArbNotifications.buildExportCompletedSpec(
            summaryText = "書き出し完了: trim-01 (120f) · share-taps.mp4",
            outputPath = "/tmp/out/trim-01",
            trimmedBundleId = "trim-01",
            originalTrashedBundleId = "src-01",
        )
        assertEquals(
            listOf(
                ArbNotificationActionId.OPEN_FOLDER,
                ArbNotificationActionId.COPY_PATH,
                ArbNotificationActionId.OPEN_VIEWER,
                ArbNotificationActionId.RESTORE_ORIGINAL,
            ),
            exportDone.actions.map { it.id },
        )

        val failed = ArbNotifications.buildActionFailedSpec("書き出し", "disk <full> & error")
        assertEquals(NotificationType.ERROR, failed.type)
        assertTrue(failed.content.contains("disk &lt;full&gt; &amp; error"))
        assertEquals(listOf(ArbNotificationActionId.OPEN_ARB_PANEL), failed.actions.map { it.id })

        val undo = ArbNotifications.buildTrashedUndoSpec("b-1")
        assertEquals(listOf(ArbNotificationActionId.UNDO_TRASH), undo.actions.map { it.id })

        for (spec in listOf(replayClosed, replayOpened, logsOnly, imageSaved, exportDone, failed, undo)) {
            assertFalse(ArbFormatters.containsEmoji(spec.title))
            assertFalse(ArbFormatters.containsEmoji(spec.content))
            for (action in spec.actions) {
                assertFalse(ArbFormatters.containsEmoji(action.label))
            }
        }
    }

    @Test
    fun localSettingsPersistsBurnTapsOnExportDefaultTrue() {
        val settings = ArbLocalSettingsState()
        assertTrue(settings.burnTapsOnExport, "burnTapsOnExport must default to true")

        settings.burnTapsOnExport = false
        val saved = settings.getState()
        assertFalse(saved.burnTapsOnExport)

        val restored = ArbLocalSettingsState()
        restored.loadState(saved)
        assertFalse(restored.burnTapsOnExport)
    }

    @Test
    fun studioControllerBurnTapsBusyStateAndExplicitCloseTabWorkCleanly() {
        var persistedBurnTaps = true
        var exports = 0
        var closedCount = 0
        val controller = ArbStudioController(
            onScrub = {},
            onSeekExact = {},
            onTrimExport = { _, _ -> exports++ },
            onPngExport = { _, _ -> },
            initialBurnTapsOnExport = true,
            onBurnTapsToggled = { persistedBurnTaps = it },
            onCloseTab = { closedCount++ },
        )

        val summary = TimelineSummaryResponse(
            bundleId = "b-studio",
            path = "/tmp/b-studio",
            format = BundleFormat.V2,
            isLegacy = false,
            provenance = Provenance.Original,
            streams = StreamsSummary(),
            unanchoredSummary = UnanchoredSummary(0, 0),
            epochs = emptyList(),
            frameCount = 60,
            keyframeCount = 2,
            logMainCount = 0,
            logUnanchoredCount = 0,
            tapMainCount = 1,
            tapUnanchoredCount = 0,
            discontinuityCount = 0,
        )
        controller.renderState(
            TimelineViewerState(
                bundleId = "b-studio",
                path = "/tmp/b-studio",
                format = BundleFormat.V2,
                isLegacy = false,
                leaseId = "lease-1",
                summary = summary,
                frameCount = 60,
            ),
            image = null,
        )

        assertTrue(controller.burnTapsOnExport)
        assertTrue(controller.isBurnTapsVisible)
        assertTrue(controller.isExportButtonEnabled)

        controller.setExporting(true, "焼き込み書き出し中...")
        assertFalse(controller.isExportButtonEnabled)
        controller.triggerExport()
        assertEquals(0, exports)
        assertEquals("焼き込み書き出し中...", controller.exportButtonLabel)

        controller.setExporting(false)
        assertTrue(controller.isExportButtonEnabled)

        controller.toggleBurnTapsOnExport(false)
        assertFalse(controller.burnTapsOnExport)
        assertFalse(persistedBurnTaps)

        controller.selectExportFormat(StudioExportFormat.WORM_BUNDLE_ONLY)
        assertFalse(controller.isBurnTapsVisible)

        // Explicit close tab button delegates cleanly and post-export TrimCloseState never nags.
        val closeState = TrimCloseState()
        closeState.initialize(totalFrames = 60, initialIn = 0, initialOut = 59)
        closeState.onRangeAdjusted(5, 40)
        assertTrue(closeState.hasUnexportedCustomTrim())
        closeState.markExported(5, 40)
        assertFalse(closeState.hasUnexportedCustomTrim(), "closing after export must never nag")

        controller.closeStudioTab()
        assertEquals(1, closedCount)
    }
}

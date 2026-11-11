package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.ReplayDevice
import io.github.shinma06.replaybuffer.core.ReplaySettings
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.settings.AppSelectionMode
import io.github.shinma06.replaybuffer.settings.settingsControls
import java.nio.file.Path
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import io.github.shinma06.replaybuffer.settings.ReplaySettings as StoredSettings

class ReplayToolWindowModelTest {
    private val device = ReplayDevice("emulator-5554", "Pixel 9", DeviceKind.EMULATOR, true)
    private val app = ApplicationTarget("com.example.app")
    private val directory = Path.of("/disposable/out")

    private fun capturing(
        state: CaptureState = CaptureState.CAPTURING,
        save: SaveSnapshot = SaveSnapshot(),
        application: ApplicationTarget = app,
        error: String? = null,
        cleanup: Int = 0,
        mode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    ) = ReplaySnapshot(1, 1, state != CaptureState.DISABLED, state,
        ReplaySettings(replaySeconds = 30, application = application, captureMode = mode), 1,
        device = device, sequenceId = "session", windowEndNs = 1_000_000_000L, save = save,
        canSave = state != CaptureState.DISABLED, saveDisabledReason = "取得を有効にしてください".takeIf { state == CaptureState.DISABLED },
        error = error, cleanupPendingCount = cleanup)

    private fun input(snapshot: ReplaySnapshot?, busy: Boolean = false, message: String? = null,
                      stored: StoredSettings = StoredSettings(retentionSeconds = 30)) = ToolWindowInput(
        snapshot = snapshot, message = message, initializationError = null, busy = busy, enabled = snapshot?.enabled == true,
        settings = stored, adb = Path.of("/sdk/adb"), adbReason = null, screenshotPackage = "com.example.app",
        screenshotDisabledReason = null, screenshotBusy = false,
    )

    @Test
    fun `normal capture is one header line with commands and nothing else expanded`() {
        val model = toolWindowModel(input(capturing()))
        assertEquals(Tone.OK, model.header.tone)
        assertEquals("●", model.header.symbol)
        assertTrue(model.header.text.contains("Pixel 9"))
        assertTrue(model.header.text.contains("com.example.app"))
        assertEquals("直前30秒を保存", model.save.label)
        assertTrue(model.save.enabled)
        assertNull(model.save.reason, "an enabled command does not explain itself")
        assertNull(model.screenshot.reason)
        assertFalse(model.showsResults)
        assertFalse(model.showsNotices)
        assertFalse(model.detailsInitiallyExpanded)
    }

    @Test
    fun `rarely needed facts stay available in collapsed details`() {
        val names = toolWindowModel(input(capturing())).details.map { it.name }
        for (name in listOf("動画の取得状態", "端末ログの取得状態", "アプリログの取得状態", "保持時間と現在の記録区間",
            "動画・ログの保存先", "クラッシュ自動保存の適用状態と直近の結果", "Android SDKの接続情報", "記録セッション"))
            assertTrue(name in names, name)
    }

    @Test
    fun `capture off uses a distinct symbol and explains the disabled save`() {
        val off = capturing(state = CaptureState.DISABLED)
        val model = toolWindowModel(input(off))
        assertEquals(Tone.OFF, model.header.tone)
        assertFalse(model.header.text.contains("Pixel 9"), "an OFF header does not claim a capture target")
        assertFalse(model.save.enabled)
        assertEquals("取得を有効にしてください", model.save.reason)
        assertFalse(model.showsNotices)
    }

    @Test
    fun `abnormal states raise notices`() {
        val unresolved = toolWindowModel(input(capturing(application = ApplicationTarget(unresolvedReason = "Run構成がありません"))))
        val notice = unresolved.notices.single()
        assertEquals(NoticeAction.OPEN_SETTINGS, notice.action)
        assertTrue(notice.text.contains("Run構成がありません"))

        val multiple = toolWindowModel(input(capturing(state = CaptureState.MULTIPLE_DEVICES, error = "端末が複数あります")))
        assertEquals(Tone.ERROR, multiple.header.tone)
        assertTrue(multiple.notices.any { it.text.contains("端末が複数あります") })

        assertEquals(Tone.WARNING, toolWindowModel(input(capturing(state = CaptureState.RECOVERING))).header.tone)
        assertTrue(toolWindowModel(input(capturing(cleanup = 2))).notices.any { it.text == cleanupDescription(2) })

        val pending = toolWindowModel(input(capturing(), stored = StoredSettings(retentionSeconds = 30, captureMode = CaptureMode.LOG_ONLY)))
        assertTrue(pending.notices.any { it.tone == Tone.PENDING && it.text.startsWith("設定の反映待ち") })

        val busy = toolWindowModel(input(capturing(), busy = true))
        assertEquals(BUSY_MESSAGE, busy.notices.single().text)
        assertFalse(busy.save.enabled)
        assertNotNull(busy.save.reason)
    }

    @Test
    fun `a completed save shows one result line with its folder`() {
        val model = toolWindowModel(input(capturing(save = SaveSnapshot(SavePhase.COMPLETED, "request", directory = directory)))
            .copy(savedAt = "10:15:30"))
        val result = model.results.single()
        assertEquals("保存しました 10:15:30", result.text)
        assertEquals(directory, result.openDirectory)
        assertFalse(model.showsFailure)
    }

    @Test
    fun `a failed save shows its reason and retry actions`() {
        val failed = SaveSnapshot(SavePhase.FAILED, "request", error = "空き容量を確認してください。", retainedPath = Path.of("/disposable/kept.partial"))
        val model = toolWindowModel(input(capturing(save = failed)))
        assertTrue(model.showsFailure)
        val failure = model.results.single().failure!!
        assertTrue(failure.reason.contains("空き容量"))
        assertTrue(failure.retainedOutput!!.contains("kept.partial"))
        assertTrue(failure.actionsEnabled)
        assertTrue(model.details.any { it.name == "固定した保存対象の端末とアプリ履歴" })
        assertFalse(toolWindowModel(input(capturing(save = failed), busy = true)).results.single().failure!!.actionsEnabled)
    }

    @Test
    fun `screenshot results are reported next to save results`() {
        val saved = toolWindowModel(input(capturing()).copy(screenshot = ScreenshotResult.Saved("10:00:00", directory.resolve("a.png"))))
        assertEquals(directory, saved.results.single().openDirectory)
        val failed = toolWindowModel(input(capturing()).copy(screenshot = ScreenshotResult.Failed("10:00:00", "端末を確認してください")))
        assertEquals(Tone.ERROR, failed.results.single().tone)
    }

    @Test
    fun `settings show recording constraints only while capture is active`() {
        val off = settingsControls(AppSelectionMode.AUTOMATIC, CaptureMode.VIDEO_AND_LOGS, true, false)
        assertTrue(off.recordingEnabled)
        assertTrue(off.videoOptionsEnabled)
        assertNull(off.recordingNotice)
        assertFalse(off.manualPackageEnabled)
        assertFalse(off.screenshotDirectoryEnabled)
        val on = settingsControls(AppSelectionMode.MANUAL, CaptureMode.VIDEO_AND_LOGS, false, true)
        assertFalse(on.recordingEnabled)
        assertFalse(on.videoOptionsEnabled)
        assertNotNull(on.recordingNotice)
        assertTrue(on.manualPackageEnabled)
        assertTrue(on.screenshotDirectoryEnabled)
        assertFalse(settingsControls(AppSelectionMode.AUTOMATIC, CaptureMode.LOG_ONLY, true, false).videoOptionsEnabled)
    }

    @Test
    fun `several devices without a choice wait for one with a single reason and a chooser`() {
        val other = ReplayDevice("emulator-5556", "Pixel 8", DeviceKind.EMULATOR, true)
        val reason = io.github.shinma06.replaybuffer.core.MULTIPLE_DEVICES_REASON
        val ambiguous = ReplaySnapshot(1, 1, true, CaptureState.MULTIPLE_DEVICES, ReplaySettings(replaySeconds = 30), 1,
            devices = listOf(device, other), error = reason)
        val model = toolWindowModel(input(ambiguous).copy(screenshotDisabledReason = "$reason。"))
        assertEquals(Tone.ERROR, model.header.tone)
        assertEquals("端末の選択待ち", model.header.text)
        assertEquals(listOf(reason), model.notices.map { it.text }, "the choice is asked once")
        assertNull(model.screenshot.reason, "the screenshot hint does not repeat the notice")
        assertFalse(model.screenshot.enabled)
        assertEquals(listOf(null, "emulator-5554", "emulator-5556"), model.devices.map { it.serial })
        assertEquals("選んでください", model.devices.first().label)
        assertTrue(model.details.any { it.text == "端末: 未選択（2台接続中）" })
        assertTrue(toolWindowModel(input(capturing())).devices.isEmpty(), "one device shows no chooser")
    }

    @Test
    fun `chooser keeps the reconnect wait and offers automatic selection like CLI and MCP`() {
        val other = ReplayDevice("emulator-5556", "Pixel 8", DeviceKind.EMULATOR, true)
        val chosen = ReplaySnapshot(1, 1, true, CaptureState.RECOVERING, ReplaySettings(deviceSerial = "emulator-5554"), 1,
            device = device.copy(connected = false), devices = listOf(other),
            error = "選択した端末（emulator-5554）の再接続を待っています。ほかの端末には切り替えません")
        val waiting = toolWindowModel(input(chosen))
        assertEquals(listOf("emulator-5554", "emulator-5556", ""), waiting.devices.map { it.serial })
        assertTrue(waiting.devices.first().label.contains("切断中"))
        assertEquals(AUTOMATIC_DEVICE_CHOICE, waiting.devices.last().label)
        assertTrue(waiting.notices.any { it.text.contains("再接続を待っています") })
        val restarted = toolWindowModel(input(chosen.copy(captureState = CaptureState.WAITING, device = null, error = null)))
        assertEquals(listOf("emulator-5554", "emulator-5556", ""), restarted.devices.map { it.serial })
        assertEquals("emulator-5554（切断中）", restarted.devices.first().label)
    }
}

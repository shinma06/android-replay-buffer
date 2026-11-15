package io.github.shinma06.replaybuffer.menubar

import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.AutoSaveReason
import io.github.shinma06.replaybuffer.core.AutoSaveSnapshot
import io.github.shinma06.replaybuffer.core.AutoSaveState
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.ReplayDevice
import io.github.shinma06.replaybuffer.core.ReplaySettings as CoreSettings
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SaveContent
import io.github.shinma06.replaybuffer.core.SaveOrigin
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveRequest
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.ui.ScreenshotResult
import java.nio.file.Path
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class MenuBarModelTest {
    private val device = ReplayDevice("emulator-5554", "Pixel 9", DeviceKind.EMULATOR, true)
    private val app = ApplicationTarget("com.example.app")
    private val directory = Path.of("/disposable/out")

    private fun snapshot(
        state: CaptureState = CaptureState.CAPTURING,
        save: SaveSnapshot = SaveSnapshot(),
        application: ApplicationTarget = app,
        error: String? = null,
        cleanup: Int = 0,
        mode: CaptureMode = CaptureMode.VIDEO_AND_LOGS,
    ) = ReplaySnapshot(1, 1, state != CaptureState.DISABLED, state,
        CoreSettings(replaySeconds = 30, application = application, captureMode = mode), 1,
        device = device, sequenceId = "session", windowEndNs = 1_000_000_000L, save = save,
        canSave = state != CaptureState.DISABLED,
        saveDisabledReason = "取得を有効にしてください".takeIf { state == CaptureState.DISABLED },
        error = error, cleanupPendingCount = cleanup)

    private fun input(
        snapshot: ReplaySnapshot?,
        name: String = "MyApp",
        busy: Boolean = false,
        message: String? = null,
        initializationError: String? = null,
        enabled: Boolean = snapshot?.enabled == true,
        stored: ReplaySettings = ReplaySettings(retentionSeconds = 30),
        screenshotReason: String? = null,
        screenshotBusy: Boolean = false,
        savedAt: String? = null,
        screenshot: ScreenshotResult? = null,
    ) = MenuProjectInput("id-$name", name, snapshot, message, initializationError, busy, enabled, stored,
        screenshotReason, screenshotBusy, savedAt, screenshot)

    private fun items(entries: List<MenuEntry>): List<MenuItemModel> = entries.flatMap {
        when (it) {
            is MenuEntry.Item -> listOf(it.item)
            is MenuEntry.Submenu -> items(it.entries)
            is MenuEntry.Separator -> emptyList()
        }
    }

    private fun item(menu: MenuBarMenu, prefix: String): MenuItemModel =
        items(menu.entries).single { it.label.startsWith(prefix) }

    @Test
    fun `icon and status line follow the seven display states`() {
        fun statusOf(snapshot: ReplaySnapshot?, screenshotBusy: Boolean = false) =
            projectStatus(input(snapshot, screenshotBusy = screenshotBusy))
        assertEquals(MenuBarIcon.CAPTURING to "取得中 · Pixel 9 · com.example.app",
            statusOf(snapshot()).let { it.icon to it.text })
        assertEquals(MenuBarIcon.OFF to "取得OFF", statusOf(snapshot(CaptureState.DISABLED)).let { it.icon to it.text })
        assertEquals(MenuBarIcon.WAITING to "端末の接続を待っています",
            statusOf(snapshot(CaptureState.WAITING)).let { it.icon to it.text })
        assertEquals(MenuBarIcon.RECOVERING to "復旧中", statusOf(snapshot(CaptureState.RECOVERING)).let { it.icon to it.text })
        assertEquals(MenuBarIcon.ATTENTION to "一部の記録を取得できていません",
            statusOf(snapshot(CaptureState.PARTIAL)).let { it.icon to it.text })
        assertEquals(MenuBarIcon.ATTENTION to "端末が複数あります",
            statusOf(snapshot(CaptureState.MULTIPLE_DEVICES)).let { it.icon to it.text })
        assertEquals(MenuBarIcon.ATTENTION to "取得エラー",
            statusOf(snapshot(error = "取得エラー")).let { it.icon to it.text })
        assertEquals(MenuBarIcon.SAVING to "保存中…",
            statusOf(snapshot(save = SaveSnapshot(SavePhase.WRITING))).let { it.icon to it.text })
        assertEquals(MenuBarIcon.SAVING to "保存中…", statusOf(snapshot(), screenshotBusy = true).let { it.icon to it.text })
        assertEquals(MenuBarIcon.FAILED to "保存に失敗しました（未解決）",
            statusOf(snapshot(save = SaveSnapshot(SavePhase.FAILED))).let { it.icon to it.text })
        assertEquals(MenuBarIcon.WAITING to "確認中…", statusOf(null).let { it.icon to it.text })
        assertEquals("取得を初期化できません",
            projectStatus(input(null, initializationError = "初期化失敗")).text)
    }

    @Test
    fun `one project inlines the menu under a title and a status line`() {
        val menu = menuBarModel(listOf(input(snapshot())))
        val labels = items(menu.entries).map { it.label }
        assertEquals("Android Replay Buffer — MyApp", labels[0])
        assertEquals("● 取得中 · Pixel 9 · com.example.app", labels[1])
        assertEquals("直前30秒を保存", labels[2])
        assertEquals("スクリーンショット", labels[3])
        assertEquals("取得する（OFFで未保存の記録を破棄）", labels[4])
        assertEquals("Android Studioで表示", labels[5])
        assertEquals("設定…", labels[6])
        assertEquals(MenuBarIcon.CAPTURING, menu.icon)
        assertEquals("Android Replay Buffer — 取得中 · Pixel 9 · com.example.app", menu.tooltip)
        assertFalse(item(menu, "Android Replay Buffer —").enabled)
        assertFalse(item(menu, "● 取得中").enabled)
        val toggle = item(menu, "取得する")
        assertTrue(toggle.enabled && toggle.checked && toggle.command == MenuBarCommand.TOGGLE_CAPTURE)
    }

    @Test
    fun `a disabled or busy save explains itself in the item label`() {
        val off = menuBarModel(listOf(input(snapshot(CaptureState.DISABLED))))
        val save = item(off, "直前")
        assertFalse(save.enabled)
        assertEquals("直前30秒を保存 — 取得を有効にしてください", save.label)

        val busy = menuBarModel(listOf(input(snapshot(CaptureState.DISABLED), busy = true)))
        assertFalse(item(busy, "直前").enabled)
        assertTrue(item(busy, "直前").label.endsWith("設定・操作を反映中です。"))
        assertFalse(item(busy, "取得する").enabled, "OFF時の反映中はトグルも無効")
        assertTrue(item(busy, "設定・操作を反映中です。").enabled, "反映中は確認への導線を出す")
        val onBusy = menuBarModel(listOf(input(snapshot(), busy = true, enabled = true)))
        assertTrue(item(onBusy, "取得する").enabled, "ON時はOFFへいつでも切り替えられる")
    }

    @Test
    fun `seconds come from the applied snapshot and fall back to stored settings`() {
        val menu = menuBarModel(listOf(input(null, stored = ReplaySettings(retentionSeconds = 45))))
        assertEquals("直前45秒を保存", item(menu, "直前").label)
    }

    @Test
    fun `screenshot follows the shared disabled reason and busy label`() {
        val disabled = menuBarModel(listOf(input(snapshot(), screenshotReason = "設定でスクリーンショットの保存先を指定してください。")))
        assertFalse(item(disabled, "スクリーンショット").enabled)
        assertTrue(item(disabled, "スクリーンショット").label.contains("保存先を指定"))
        val busy = menuBarModel(listOf(input(snapshot(), screenshotBusy = true, screenshotReason = "スクリーンショットを保存中です。")))
        assertEquals("スクショを保存中…", item(busy, "スクショ").label)
        assertFalse(item(busy, "スクショ").enabled)
    }

    @Test
    fun `a completed save offers its folder and a failed save leads to the IDE`() {
        val completed = menuBarModel(listOf(input(snapshot(save = SaveSnapshot(SavePhase.COMPLETED, "r", directory = directory)),
            savedAt = "10:21:05")))
        val open = item(completed, "保存しました")
        assertEquals("保存しました 10:21:05 — フォルダを開く", open.label)
        assertEquals(MenuBarCommand.OPEN_DIRECTORY, open.command)
        assertEquals(directory, open.directory)

        val failed = menuBarModel(listOf(input(snapshot(save = SaveSnapshot(SavePhase.FAILED, "r")))))
        assertEquals(MenuBarIcon.FAILED, failed.icon)
        assertEquals(MenuBarCommand.SHOW_IDE, item(failed, "保存に失敗しました — Android Studioで確認…").command)
    }

    @Test
    fun `screenshot results appear beside the save result`() {
        val saved = menuBarModel(listOf(input(snapshot(),
            screenshot = ScreenshotResult.Saved("10:00:00", directory.resolve("a.png")))))
        val open = item(saved, "スクリーンショットを保存しました")
        assertEquals(MenuBarCommand.OPEN_DIRECTORY, open.command)
        assertEquals(directory, open.directory)
        val failed = menuBarModel(listOf(input(snapshot(),
            screenshot = ScreenshotResult.Failed("10:00:00", "端末を確認してください"))))
        assertEquals(MenuBarCommand.SHOW_IDE, item(failed, "スクリーンショットを保存できませんでした").command)
    }

    @Test
    fun `attention rows appear only while something needs it`() {
        val calm = items(menuBarModel(listOf(input(snapshot()))).entries).map { it.label }
        assertEquals(7, calm.size, "通常時は見出し・状態・3操作・IDE導線だけ")

        val warned = menuBarModel(listOf(input(snapshot(), message = "取得エラー")))
        assertEquals(MenuBarCommand.SHOW_IDE, item(warned, "取得エラー").command)

        val unresolved = menuBarModel(listOf(input(snapshot(application = ApplicationTarget(unresolvedReason = "Run構成がありません")))))
        val settings = item(unresolved, "アプリ: Run構成がありません")
        assertEquals(MenuBarCommand.OPEN_SETTINGS, settings.command)
        assertEquals("アプリ: Run構成がありません — 設定を開く…", settings.label)

        val pending = menuBarModel(listOf(input(snapshot(), stored = ReplaySettings(retentionSeconds = 30, captureMode = CaptureMode.LOG_ONLY))))
        assertTrue(item(pending, "設定の反映待ち").label.startsWith("設定の反映待ち: 取得モード"))

        val cleanup = menuBarModel(listOf(input(snapshot(cleanup = 2))))
        assertTrue(item(cleanup, "前回の端末処理の片付け待ち").label.contains("2件"))
    }

    @Test
    fun `suspended crash autosave offers its reason and project IDE without changing manual controls`() {
        val prefix = "クラッシュ自動保存は停止中です"
        listOf(AutoSaveReason.LIMIT_REACHED.description, AutoSaveReason.DETECTOR_OVERFLOW.description, null).forEach { reason ->
            val snapshot = snapshot().copy(autoSave = AutoSaveSnapshot(AutoSaveState.SUSPENDED, reason))
            val menu = menuBarModel(listOf(input(snapshot)))
            val attention = item(menu, prefix)
            assertEquals(prefix + reason?.let { ": $it" }.orEmpty() + " — Android Studioで確認…", attention.label)
            assertTrue(attention.enabled)
            assertEquals(MenuBarCommand.SHOW_IDE, attention.command)
            assertEquals("id-MyApp", attention.projectId)
            val ops = FakeOps()
            attention.run(ops)
            assertEquals(listOf("showToolWindow"), ops.calls)
            assertTrue(item(menu, "直前").enabled)
            assertTrue(item(menu, "取得する").checked)
            assertEquals(MenuBarIcon.CAPTURING, menu.icon)
        }
        listOf(AutoSaveState.OFF, AutoSaveState.WAITING, AutoSaveState.ARMED).forEach { state ->
            val snapshot = snapshot().copy(autoSave = AutoSaveSnapshot(state, "正常な状態の説明"))
            assertFalse(items(menuBarModel(listOf(input(snapshot))).entries).any { it.label.startsWith(prefix) })
        }
    }

    @Test
    fun `several projects become labelled submenus and share the highest priority icon`() {
        val menu = menuBarModel(listOf(
            input(snapshot(), name = "MyApp"),
            input(snapshot(CaptureState.DISABLED, save = SaveSnapshot(SavePhase.FAILED)), name = "Other"),
        ))
        assertEquals(MenuBarIcon.FAILED, menu.icon, "最も優先の高い状態をアイコンにする")
        val submenus = menu.entries.filterIsInstance<MenuEntry.Submenu>()
        assertEquals(listOf("MyApp ● 取得中", "Other ▲ 保存失敗"), submenus.map { it.title })
        assertEquals("Android Replay Buffer", items(menu.entries).first().label)
        val first = items(submenus[0].entries)
        assertEquals("● 取得中 · Pixel 9 · com.example.app", first.first().label)
        assertEquals("id-MyApp", first.single { it.command == MenuBarCommand.SAVE }.projectId)
        assertEquals("id-Other", items(submenus[1].entries).single { it.command == MenuBarCommand.SAVE }.projectId)
    }

    @Test
    fun `menu commands only reach the existing operations`() {
        val ops = FakeOps()
        item(menuBarModel(listOf(input(snapshot()))), "直前").run(ops)
        assertEquals(SaveRequest(SaveContent.ALL, SaveOrigin.MENU_BAR), ops.savedRequest)

        item(menuBarModel(listOf(input(snapshot()))), "スクリーンショット").run(ops)
        assertEquals(listOf("save", "screenshot"), ops.calls)

        val on = FakeOps(captureEnabled = true)
        item(menuBarModel(listOf(input(snapshot()))), "取得する").copy(checked = false).run(on)
        assertFalse(on.captureEnabled, "ONの項目はOFFへ切り替える")
        val off = FakeOps(captureEnabled = false)
        item(menuBarModel(listOf(input(snapshot(CaptureState.DISABLED)))), "取得する").copy(checked = true).run(off)
        assertTrue(off.captureEnabled)

        item(menuBarModel(listOf(input(snapshot()))), "Android Studioで表示").run(ops)
        item(menuBarModel(listOf(input(snapshot()))), "設定…").run(ops)
        assertEquals(listOf("save", "screenshot", "showToolWindow", "openSettings"), ops.calls)

        val completed = input(snapshot(save = SaveSnapshot(SavePhase.COMPLETED, "r", directory = directory)))
        item(menuBarModel(listOf(completed)), "保存しました").run(ops)
        assertEquals(directory, ops.opened)
    }

    @Test
    fun `capture selection is applied even if another entry point already changed capture`() {
        val toggle = item(menuBarModel(listOf(input(snapshot()))), "取得する")
        listOf(false, true).forEach { current ->
            listOf(false, true).forEach { selected ->
                val ops = FakeOps(captureEnabled = current)
                toggle.copy(checked = selected).run(ops)
                assertEquals(selected, ops.captureEnabled)
            }
        }
    }

    private class FakeOps(var captureEnabled: Boolean = true) : MenuBarOperations {
        val calls = mutableListOf<String>()
        var savedRequest: SaveRequest? = null
        var opened: Path? = null
        override fun setEnabled(value: Boolean) { calls += "setEnabled($value)"; captureEnabled = value }
        override fun save(request: SaveRequest) { calls += "save"; savedRequest = request }
        override fun screenshot() { calls += "screenshot" }
        override fun openDirectory(directory: Path) { calls += "openDirectory"; opened = directory }
        override fun showToolWindow() { calls += "showToolWindow" }
        override fun openSettings() { calls += "openSettings" }
    }
}

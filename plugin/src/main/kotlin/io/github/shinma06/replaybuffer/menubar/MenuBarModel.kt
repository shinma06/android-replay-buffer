package io.github.shinma06.replaybuffer.menubar

import io.github.shinma06.replaybuffer.core.AutoSaveState
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SaveOrigin
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveRequest
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.ui.ScreenshotResult
import io.github.shinma06.replaybuffer.ui.cleanupDescription
import io.github.shinma06.replaybuffer.ui.pendingSettingsDescription
import java.nio.file.Path

/** Tray icon kinds in the §2.5 display priority order; the enum order is that priority. */
internal enum class MenuBarIcon(val symbol: String, val shortLabel: String) {
    FAILED("▲", "保存失敗"),
    SAVING("▼", "保存中"),
    ATTENTION("◉", "要確認"),
    RECOVERING("↻", "復旧中"),
    WAITING("◌", "接続待ち"),
    CAPTURING("●", "取得中"),
    OFF("○", "取得OFF"),
}

/** Commands a menu entry can trigger; every one maps to an existing ReplayProjectService operation. */
internal enum class MenuBarCommand { SAVE, SCREENSHOT, TOGGLE_CAPTURE, OPEN_DIRECTORY, SHOW_IDE, OPEN_SETTINGS }

/** What one project's menu entries may call; implemented over ReplayProjectService, so the menu adds no capture path. */
internal interface MenuBarOperations {
    fun setEnabled(value: Boolean)
    fun save(request: SaveRequest)
    fun screenshot()
    fun openDirectory(directory: Path)
    fun showToolWindow()
    fun openSettings()
}

/** One menu row. Items without a command are display-only and always disabled. */
internal data class MenuItemModel(
    val label: String,
    val enabled: Boolean = false,
    val command: MenuBarCommand? = null,
    val checked: Boolean = false,
    val projectId: String? = null,
    val directory: Path? = null,
) {
    fun run(ops: MenuBarOperations) {
        when (command) {
            MenuBarCommand.SAVE -> ops.save(SaveRequest(origin = SaveOrigin.MENU_BAR))
            MenuBarCommand.SCREENSHOT -> ops.screenshot()
            MenuBarCommand.TOGGLE_CAPTURE -> ops.setEnabled(checked)
            MenuBarCommand.OPEN_DIRECTORY -> directory?.let(ops::openDirectory)
            MenuBarCommand.SHOW_IDE -> ops.showToolWindow()
            MenuBarCommand.OPEN_SETTINGS -> ops.openSettings()
            null -> {}
        }
    }
}

internal sealed interface MenuEntry {
    data class Item(val item: MenuItemModel) : MenuEntry
    data class Submenu(val title: String, val entries: List<MenuEntry>) : MenuEntry
    data object Separator : MenuEntry
}

/** The full tray presentation: aggregate icon, tooltip and the menu tree to render. */
internal data class MenuBarMenu(val icon: MenuBarIcon, val tooltip: String, val entries: List<MenuEntry>)

/** Everything one project contributes to the menu, read from its services on the EDT. */
internal data class MenuProjectInput(
    val projectId: String,
    val name: String,
    val snapshot: ReplaySnapshot?,
    val message: String?,
    val initializationError: String?,
    val busy: Boolean,
    val enabled: Boolean,
    val settings: ReplaySettings,
    val screenshotDisabledReason: String?,
    val screenshotBusy: Boolean,
    val savedAt: String?,
    val screenshot: ScreenshotResult?,
    val location: String,
)

internal data class ProjectStatus(val icon: MenuBarIcon, val text: String)

/** The state shown by the icon shape and repeated as text at the top of the menu (docs/requirements/menubar.md §2.5). */
internal fun projectStatus(input: MenuProjectInput): ProjectStatus {
    val snapshot = input.snapshot
    return when {
        snapshot?.save?.phase == SavePhase.FAILED -> ProjectStatus(MenuBarIcon.FAILED, "保存に失敗しました（未解決）")
        snapshot?.save?.phase == SavePhase.WRITING || input.screenshotBusy -> ProjectStatus(MenuBarIcon.SAVING, "保存中…")
        snapshot == null -> ProjectStatus(
            if (input.initializationError != null) MenuBarIcon.ATTENTION else MenuBarIcon.WAITING,
            if (input.initializationError != null) "取得を初期化できません" else "確認中…")
        snapshot.captureState == CaptureState.PARTIAL -> ProjectStatus(MenuBarIcon.ATTENTION, "一部の記録を取得できていません")
        snapshot.captureState == CaptureState.MULTIPLE_DEVICES -> ProjectStatus(MenuBarIcon.ATTENTION, "端末が複数あります")
        snapshot.error != null -> ProjectStatus(MenuBarIcon.ATTENTION, snapshot.error)
        snapshot.captureState == CaptureState.RECOVERING -> ProjectStatus(MenuBarIcon.RECOVERING, "復旧中")
        snapshot.captureState == CaptureState.WAITING -> ProjectStatus(MenuBarIcon.WAITING, "端末の接続を待っています")
        snapshot.captureState == CaptureState.CAPTURING -> ProjectStatus(MenuBarIcon.CAPTURING,
            listOfNotNull("取得中", snapshot.device?.name, snapshot.settings.application.packageName).joinToString(" · "))
        else -> ProjectStatus(MenuBarIcon.OFF, "取得OFF")
    }
}

private const val BUSY_REASON = "設定・操作を反映中です。"

private fun saveItem(input: MenuProjectInput): MenuItemModel {
    val snapshot = input.snapshot
    val seconds = snapshot?.settings?.replaySeconds ?: input.settings.retentionSeconds
    val reason = if (input.busy) BUSY_REASON else snapshot?.saveDisabledReason
    val enabled = !input.busy && snapshot?.canSave == true
    return MenuItemModel(
        "直前${seconds}秒を保存" + if (!enabled && !reason.isNullOrBlank()) " — $reason" else "",
        enabled, MenuBarCommand.SAVE, projectId = input.projectId)
}

private fun screenshotItem(input: MenuProjectInput): MenuItemModel {
    val reason = input.screenshotDisabledReason
    val label = if (input.screenshotBusy) "スクショを保存中…"
        else "スクリーンショット" + if (reason != null) " — $reason" else ""
    return MenuItemModel(label, reason == null, MenuBarCommand.SCREENSHOT, projectId = input.projectId)
}

private fun toggleItem(input: MenuProjectInput): MenuItemModel = MenuItemModel(
    "取得する（OFFで未保存の記録を破棄）", input.enabled || !input.busy,
    MenuBarCommand.TOGGLE_CAPTURE, checked = input.enabled, projectId = input.projectId)

/** Last-result and attention rows; each carries its own lead into the IDE like the ToolWindow links. */
private fun attentionItems(input: MenuProjectInput): List<MenuItemModel> = buildList {
    val snapshot = input.snapshot
    when (snapshot?.save?.phase) {
        SavePhase.COMPLETED -> snapshot.save.directory?.let { directory ->
            add(MenuItemModel("保存しました${input.savedAt?.let { " $it" }.orEmpty()} — フォルダを開く",
                enabled = true, command = MenuBarCommand.OPEN_DIRECTORY, projectId = input.projectId, directory = directory))
        }
        SavePhase.FAILED -> add(MenuItemModel("保存に失敗しました — Android Studioで確認…",
            enabled = true, command = MenuBarCommand.SHOW_IDE, projectId = input.projectId))
        else -> {}
    }
    when (val shot = input.screenshot) {
        is ScreenshotResult.Saved -> add(MenuItemModel(
            "スクリーンショットを保存しました ${shot.time} — フォルダを開く",
            enabled = true, command = MenuBarCommand.OPEN_DIRECTORY, projectId = input.projectId, directory = shot.file.parent))
        is ScreenshotResult.Failed -> add(MenuItemModel(
            "スクリーンショットを保存できませんでした ${shot.time} — Android Studioで確認…",
            enabled = true, command = MenuBarCommand.SHOW_IDE, projectId = input.projectId))
        null -> {}
    }
    val message = if (input.busy) BUSY_REASON else input.message ?: input.initializationError ?: snapshot?.error
    if (!message.isNullOrBlank()) add(MenuItemModel("$message — Android Studioで確認…",
        enabled = true, command = MenuBarCommand.SHOW_IDE, projectId = input.projectId))
    snapshot?.autoSave?.takeIf { it.state == AutoSaveState.SUSPENDED }?.let { autoSave ->
        val reason = autoSave.reason?.takeIf { it.isNotBlank() }?.let { ": $it" }.orEmpty()
        add(MenuItemModel("クラッシュ自動保存は停止中です$reason — Android Studioで確認…",
            enabled = true, command = MenuBarCommand.SHOW_IDE, projectId = input.projectId))
    }
    val app = snapshot?.settings?.application
    if (snapshot?.enabled == true && app?.packageName == null && !app?.unresolvedReason.isNullOrBlank())
        add(MenuItemModel("アプリ: ${app.unresolvedReason} — 設定を開く…",
            enabled = true, command = MenuBarCommand.OPEN_SETTINGS, projectId = input.projectId))
    pendingSettingsDescription(snapshot?.settings, input.settings)?.let {
        add(MenuItemModel("$it — Android Studioで確認…", enabled = true, command = MenuBarCommand.SHOW_IDE, projectId = input.projectId))
    }
    cleanupDescription(snapshot?.cleanupPendingCount ?: 0).takeIf { it.isNotEmpty() }?.let {
        add(MenuItemModel("$it — Android Studioで確認…", enabled = true, command = MenuBarCommand.SHOW_IDE, projectId = input.projectId))
    }
}

/** One project's rows; a single project inlines them at the top level, several sit inside their submenus. */
internal fun projectEntries(input: MenuProjectInput): List<MenuEntry> = buildList {
    val status = projectStatus(input)
    add(MenuEntry.Item(MenuItemModel("${status.icon.symbol} ${status.text}")))
    add(MenuEntry.Separator)
    add(MenuEntry.Item(saveItem(input)))
    add(MenuEntry.Item(screenshotItem(input)))
    val attention = attentionItems(input)
    if (attention.isNotEmpty()) {
        add(MenuEntry.Separator)
        attention.forEach { add(MenuEntry.Item(it)) }
    }
    add(MenuEntry.Separator)
    add(MenuEntry.Item(toggleItem(input)))
    add(MenuEntry.Separator)
    add(MenuEntry.Item(MenuItemModel("Android Studioで表示", enabled = true, command = MenuBarCommand.SHOW_IDE, projectId = input.projectId)))
    add(MenuEntry.Item(MenuItemModel("設定…", enabled = true, command = MenuBarCommand.OPEN_SETTINGS, projectId = input.projectId)))
}

/**
 * Builds the whole menu from per-project inputs. One project is inlined under a title row; several become labelled
 * submenus so the target is chosen by name, never guessed from the front window.
 */
internal fun menuBarModel(projects: List<MenuProjectInput>): MenuBarMenu {
    val statuses = projects.map(::projectStatus)
    val icon = statuses.minOf { it.icon }
    val nameCounts = projects.groupingBy { it.name }.eachCount()
    val entries = buildList {
        if (projects.size == 1) {
            val input = projects.single()
            add(MenuEntry.Item(MenuItemModel("Android Replay Buffer — ${input.name}")))
            addAll(projectEntries(input))
        } else {
            add(MenuEntry.Item(MenuItemModel("Android Replay Buffer")))
            add(MenuEntry.Separator)
            projects.forEach { input ->
                val status = projectStatus(input)
                val name = if (nameCounts.getValue(input.name) > 1) "${input.name} — ${input.location}" else input.name
                add(MenuEntry.Submenu("$name ${status.icon.symbol} ${status.icon.shortLabel}", projectEntries(input)))
            }
        }
    }
    val tooltip = "Android Replay Buffer" + statuses.singleOrNull()?.let { " — ${it.text}" }.orEmpty()
    return MenuBarMenu(icon, tooltip, entries)
}

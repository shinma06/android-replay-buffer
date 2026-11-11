package io.github.shinma06.replaybuffer.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowManagerListener
import com.intellij.ui.JBColor
import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.core.StreamSnapshot
import io.github.shinma06.replaybuffer.core.StreamState
import io.github.shinma06.replaybuffer.ide.LogcatTargetDevice
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.ide.streamName
import io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurable
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import org.jetbrains.jewel.bridge.JewelComposePanel
import org.jetbrains.jewel.bridge.toComposeColor
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.component.CheckboxRow
import org.jetbrains.jewel.ui.component.DefaultButton
import org.jetbrains.jewel.ui.component.Link
import org.jetbrains.jewel.ui.component.OutlinedButton
import org.jetbrains.jewel.ui.component.Text
import org.jetbrains.jewel.ui.component.Tooltip
import org.jetbrains.jewel.ui.component.VerticallyScrollableContainer
import java.awt.BorderLayout
import java.nio.file.Path
import java.time.LocalTime
import java.time.format.DateTimeFormatter
import java.util.Locale
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CompletionException
import javax.swing.JComponent
import javax.swing.JPanel
import javax.swing.SwingUtilities
import kotlin.math.ceil

class ReplayToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val controller = ReplayToolWindowController(project)
        val panel = JPanel(BorderLayout()).apply {
            add(JewelComposePanel { ReplayToolWindowContent(controller) }, BorderLayout.CENTER)
        }
        val content = ContentFactoryHolder.create(panel)
        Disposer.register(content, controller)
        toolWindow.contentManager.addContent(content)
    }
}

private object ContentFactoryHolder {
    fun create(component: JComponent) = com.intellij.ui.content.ContentFactory.getInstance().createContent(component, "", false)
}

private val TIME: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm:ss", Locale.ROOT)

/** Owns the service subscriptions and commands; the composable only renders [model]. */
internal class ReplayToolWindowController(private val project: Project) : Disposable {
    private val service = project.getService(ReplayProjectService::class.java)
    private val store = project.getService(ReplaySettingsStore::class.java)
    private val toolWindowManager = project.getService(ToolWindowManager::class.java)
    private var disposed = false
    private var shownSave: SaveSnapshot? = null
    private var savedKey: Pair<String?, Long>? = null
    private var savedAt: String? = null
    private var screenshotResult: ScreenshotResult? = null
    private var trackedScreenshot: CompletableFuture<*>? = null
    private val alive: Boolean get() = !disposed && !project.isDisposed

    /** Logic-only holder: its click and refresh behaviour stays the tested Swing implementation; it is never shown. */
    val logcatButton = LogcatToolWindowButton({ alive }, { service.captureView.snapshot?.device }, { window, serial, active, deadlineNs ->
        // Guard before loading the version-bound Java bridge; close remains available on other IDE builds.
        val reason = logcatTargetBuildReason(ApplicationInfo.getInstance().build.asString())
        if (reason != null) CompletableFuture.completedFuture(reason)
        else LogcatTargetDevice.prepare(window, serial, active, deadlineNs, window.isVisible)
    }, { window ->
        logcatTargetBuildReason(ApplicationInfo.getInstance().build.asString()) == null && LogcatTargetDevice.needsTab(window)
    }) {
        toolWindowManager?.getToolWindow("Logcat")
    }

    var model by mutableStateOf(toolWindowModel(input()))
        private set

    init {
        Disposer.register(this, logcatButton)
        logcatButton.addPropertyChangeListener { event ->
            if (event.propertyName in setOf("text", "enabled", JComponent.TOOL_TIP_TEXT_KEY) && alive) publish()
        }
        val refresh = Runnable {
            if (alive) ToolWindowManager.getInstance(project).invokeLater { if (alive) render() }
        }
        val connection = project.messageBus.connect(this)
        connection.subscribe(ReplayProjectService.ENVIRONMENT_CHANGED, refresh)
        connection.subscribe(ReplaySettingsStore.CHANGED, refresh)
        val refreshLogcat = Runnable {
            if (alive) toolWindowManager?.invokeLater { if (alive) logcatButton.refresh() }
        }
        connection.subscribe(ToolWindowManagerListener.TOPIC, object : ToolWindowManagerListener {
            override fun toolWindowShown(toolWindow: ToolWindow) {
                if (alive && toolWindow.id == "Logcat") {
                    // Retain the show event even if the IDE hides the window again before a queued refresh.
                    val cancel = Runnable { if (alive) logcatButton.cancelOpen() }
                    if (SwingUtilities.isEventDispatchThread()) cancel.run() else toolWindowManager?.invokeLater(cancel)
                }
            }

            override fun stateChanged(toolWindowManager: ToolWindowManager) = refreshLogcat.run()

            override fun toolWindowsRegistered(ids: List<String>, toolWindowManager: ToolWindowManager) = refreshLogcat.run()

            override fun toolWindowUnregistered(id: String, toolWindow: ToolWindow) = refreshLogcat.run()
        })
        // The first save already on screen predates this panel, so its time is unknown.
        savedKey = service.captureView.snapshot?.save?.saveKey()
        render()
    }

    private fun SaveSnapshot.saveKey(): Pair<String?, Long>? = takeIf { it.phase == SavePhase.COMPLETED }?.let { it.requestId to it.attempt }

    private fun input(): ToolWindowInput {
        val view = service.captureView
        val snapshot = view.snapshot
        snapshot?.save?.saveKey()?.let { key ->
            if (key != savedKey) { savedKey = key; savedAt = LocalTime.now().format(TIME) }
        }
        return ToolWindowInput(
            snapshot = snapshot,
            message = view.message,
            initializationError = view.initializationError,
            busy = service.resolving || view.pending > 0,
            enabled = store.enabled,
            settings = store.settings(),
            adb = service.environment?.adb,
            adbReason = service.environment?.adbReason,
            screenshotPackage = service.environment?.application?.packageName,
            screenshotDisabledReason = service.screenshotDisabledReason,
            screenshotBusy = service.screenshotBusy,
            logcat = LogcatState(logcatButton.text.orEmpty(), logcatButton.isEnabled, logcatButton.toolTipText.orEmpty(), logcatButton.failed),
            savedAt = savedAt.takeIf { snapshot?.save?.phase == SavePhase.COMPLETED },
            screenshot = screenshotResult,
        )
    }

    fun render() {
        trackScreenshot()
        logcatButton.refresh()
        publish()
    }

    private fun publish() {
        shownSave = service.captureView.snapshot?.save
        model = toolWindowModel(input())
    }

    fun setEnabled(enabled: Boolean) { if (alive) service.setEnabled(enabled) }

    fun save() { if (alive) service.save() }

    fun screenshot() {
        if (!alive) return
        service.screenshot()
        render()
    }

    /** Shows the newest screenshot result whichever entry point started it. */
    private fun trackScreenshot() {
        val latest = service.latestScreenshot ?: return
        if (latest === trackedScreenshot) return
        trackedScreenshot = latest
        latest.thenAccept { outcome ->
            val time = outcome.time.format(TIME)
            val saved = outcome.saved
            val result = if (saved != null) ScreenshotResult.Saved(time, saved.file) else {
                val cause = (outcome.failure as? CompletionException)?.cause ?: outcome.failure
                ScreenshotResult.Failed(time, cause?.message ?: "スクリーンショットを保存できませんでした。")
            }
            SwingUtilities.invokeLater { if (alive && trackedScreenshot === latest) { screenshotResult = result; render() } }
        }
    }

    fun toggleLogcat() { if (alive && logcatButton.isEnabled) logcatButton.doClick(0) }

    fun openSettings() {
        if (alive) ShowSettingsUtil.getInstance().showSettingsDialog(project, ReplaySettingsConfigurable::class.java)
    }

    fun openDirectory(directory: Path) { if (alive) service.openDirectory(directory) }

    private fun failedId(): String? = shownSave?.takeIf { it.phase == SavePhase.FAILED }?.requestId

    fun retry() { if (alive) failedId()?.let(service::retry) }

    fun discard() { if (alive) failedId()?.let(service::discard) }

    fun retryElsewhere() {
        val id = if (alive) failedId() else null
        id ?: return
        FileChooser.chooseFile(FileChooserDescriptorFactory.createSingleFolderDescriptor(), project, null) { selected ->
            if (!alive) return@chooseFile
            val path = selected.toNioPath()
            val answer = Messages.showYesNoDialog(project,
                "この保存先で、固定した保存対象を再試行します。\n$path\n\n通常設定の保存先は変更しません。",
                "保存先を変更して再試行", "適用して再試行", "キャンセル", Messages.getQuestionIcon())
            if (alive && answer == Messages.YES) service.retryAtDirectory(id, path)
        }
    }

    override fun dispose() { disposed = true }
}

private val OK_COLOR = JBColor(java.awt.Color(0x1F7A33), java.awt.Color(0x5FB865))
private val PENDING_COLOR = JBColor(java.awt.Color(0x3574F0), java.awt.Color(0x548AF7))
private val WARNING_COLOR = JBColor(java.awt.Color(0xA36200), java.awt.Color(0xE0A03D))
private val ERROR_COLOR = JBColor(java.awt.Color(0xC62828), java.awt.Color(0xF06A6A))

@Composable
private fun toneColor(tone: Tone): Color = when (tone) {
    Tone.OK -> OK_COLOR.toComposeColor()
    Tone.PENDING -> PENDING_COLOR.toComposeColor()
    Tone.WARNING -> WARNING_COLOR.toComposeColor()
    Tone.ERROR -> ERROR_COLOR.toComposeColor()
    Tone.OFF -> JewelTheme.globalColors.text.info
}

@Composable
private fun Hint(text: String, modifier: Modifier = Modifier) {
    Text(text, modifier = modifier, color = JewelTheme.globalColors.text.info)
}

@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
private fun WithTooltip(tooltip: String, content: @Composable () -> Unit) {
    if (tooltip.isBlank()) content() else Tooltip(tooltip = { Text(tooltip) }, content = content)
}

/** The headline takes only the width left after the capture switch, so the switch is never squeezed out. */
@Composable
internal fun StatusHeaderRow(heading: @Composable () -> Unit, toggle: @Composable () -> Unit) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f)) { heading() }
        toggle()
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ReplayToolWindowContent(controller: ReplayToolWindowController) {
    val model = controller.model
    VerticallyScrollableContainer(modifier = Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxWidth().padding(12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            // 1. Status headline with the capture switch at the end.
            StatusHeaderRow(
                heading = {
                    // A long device or package name is cut on screen; the tooltip and description keep it whole.
                    WithTooltip(listOf(model.header.text, model.header.tooltip).filter { it.isNotBlank() }.joinToString("\n")) {
                        Text("${model.header.symbol} ${model.header.text}",
                            modifier = Modifier.semantics { heading(); contentDescription = "取得状態: ${model.header.text}" },
                            color = toneColor(model.header.tone), fontWeight = FontWeight.Bold,
                            maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                },
                toggle = {
                    WithTooltip(model.toggle.tooltip) {
                        CheckboxRow("取得", model.toggle.selected, { controller.setEnabled(it) }, enabled = model.toggle.enabled,
                            modifier = Modifier.semantics { contentDescription = "取得を有効にする" })
                    }
                },
            )
            // 2. Main commands; only a disabled command explains itself.
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    WithTooltip(model.save.tooltip) {
                        DefaultButton({ controller.save() }, enabled = model.save.enabled,
                            modifier = Modifier.semantics { contentDescription = model.save.label }) { Text(model.save.label) }
                    }
                    WithTooltip(model.screenshot.tooltip) {
                        OutlinedButton({ controller.screenshot() }, enabled = model.screenshot.enabled,
                            modifier = Modifier.semantics { contentDescription = "現在画面のスクリーンショットを保存" }) { Text(model.screenshot.label) }
                    }
                    WithTooltip(model.logcat.tooltip) {
                        OutlinedButton({ controller.toggleLogcat() }, enabled = model.logcat.enabled,
                            modifier = Modifier.semantics { contentDescription = model.logcat.label }) { Text(model.logcat.label) }
                    }
                }
                model.save.reason?.let { Hint("保存: $it") }
                model.screenshot.reason?.let { Hint("スクリーンショット: $it") }
                model.logcat.reason?.let { Hint("Logcat: $it") }
            }
            // 3. Recent results, only after a save or screenshot.
            model.results.forEach { RecentResultView(it, controller) }
            // 4. Notices, only while something needs attention.
            model.notices.forEach { notice ->
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(toneSymbol(notice.tone), color = toneColor(notice.tone))
                    Column(Modifier.weight(1f)) {
                        Text(notice.text)
                        if (notice.action == NoticeAction.OPEN_SETTINGS) Link("設定を開く", { controller.openSettings() })
                    }
                }
            }
            // 5. Details, collapsed until asked for.
            var expanded by remember { mutableStateOf(model.detailsInitiallyExpanded) }
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text((if (expanded) "▼ " else "▶ ") + "詳細",
                    modifier = Modifier.clickable { expanded = !expanded }
                        .semantics { contentDescription = if (expanded) "詳細を閉じる" else "詳細を開く" },
                    fontWeight = FontWeight.Bold)
                if (expanded) SelectionContainer {
                    Column(Modifier.padding(start = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        model.details.forEach { Text(it.text, modifier = Modifier.semantics { contentDescription = it.name }) }
                    }
                }
            }
            // 6. Settings.
            WithTooltip(SETTINGS_TOOLTIP) { Link("設定…", { controller.openSettings() }) }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RecentResultView(result: RecentResult, controller: ReplayToolWindowController) {
    val failure = result.failure
    val frame = if (failure != null) Modifier.fillMaxWidth()
        .border(1.dp, toneColor(Tone.ERROR), RoundedCornerShape(4.dp))
        .background(toneColor(Tone.ERROR).copy(alpha = 0.06f), RoundedCornerShape(4.dp))
        .padding(8.dp) else Modifier.fillMaxWidth()
    Column(frame, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.Center) {
            Text("${toneSymbol(result.tone)} ${result.text}", color = toneColor(result.tone),
                fontWeight = if (failure != null) FontWeight.Bold else null)
            result.openDirectory?.let { directory ->
                WithTooltip(directory.toString()) { Link("フォルダを開く", { controller.openDirectory(directory) }) }
            }
        }
        result.secondary.forEach { Hint(it) }
        if (failure != null) {
            Text(failure.reason)
            Hint(failure.fixedRequest)
            failure.retainedOutput?.let { Hint(it) }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                WithTooltip(RETRY_TOOLTIP) {
                    DefaultButton({ controller.retry() }, enabled = failure.actionsEnabled) { Text("同じ対象を再試行") }
                }
                WithTooltip(RETRY_TOOLTIP) {
                    OutlinedButton({ controller.retryElsewhere() }, enabled = failure.actionsEnabled) { Text("別の保存先で再試行…") }
                }
                WithTooltip(RETRY_TOOLTIP) {
                    OutlinedButton({ controller.discard() }, enabled = failure.actionsEnabled) { Text("未保存の対象を破棄") }
                }
            }
            Hint(DISCARD_WARNING)
        }
    }
}

internal fun ReplaySnapshot.captureDescription(): String = when (captureState) {
    CaptureState.DISABLED -> "無効 — 未保存の記録はありません"
    CaptureState.WAITING -> "接続待ち"
    CaptureState.CAPTURING -> "取得中"
    CaptureState.RECOVERING -> if (frozen) "接続待ち — 切断前の記録を保持" else "復旧中"
    CaptureState.PARTIAL -> "一部取得失敗"
    CaptureState.MULTIPLE_DEVICES -> "初期版は1台に対応しています。ほかの端末を切断してください。"
}

internal fun streamDescription(label: String, stream: StreamSnapshot?): String {
    if (stream == null) return "$label: 確認中…"
    val state = when (stream.state) {
        StreamState.WAITING -> "待機"
        StreamState.CAPTURING -> "取得中"
        StreamState.RECOVERING -> "復旧中"
        StreamState.UNAVAILABLE -> "未取得"
        StreamState.NOT_REQUESTED -> return "$label: 対象外（ログ専用）"
    }
    return "$label: $state / ${String.format(Locale.ROOT, "%.1f", stream.availableSeconds)}秒分" +
        (stream.reason?.let { " / $it" } ?: "") +
        if (stream.gaps.isEmpty()) "" else " / 現在窓に不足・未確認${stream.gaps.size}件（下の詳細）"
}

/** Describe the core's window-filtered original gaps; presentation must not recompute time membership. */
internal fun currentGapDescription(snapshot: ReplaySnapshot?): String {
    if (snapshot == null) return ""
    val gaps = (snapshot.video.gaps + snapshot.deviceLog.gaps + snapshot.appLog.gaps).distinct()
    if (gaps.isEmpty()) return ""
    return "現在窓内の不足・未確認（記録時刻）:\n" + gaps.joinToString("\n") { gap ->
        val kind = if (gap.stream == "clock") "時計" else streamName(gap.stream)
        val boundary = when (gap.boundaryUncertaintyNs) {
            null, Long.MAX_VALUE -> " / 境界の誤差は未確定"
            0L -> ""
            else -> " / 境界の誤差±${recordTime(gap.boundaryUncertaintyNs)}"
        }
        "$kind: ${recordTime(gap.fromNs)}〜${recordTime(gap.toNs)} / ${gap.reason}$boundary"
    }
}

internal fun frozenTargetDescription(save: SaveSnapshot?): String {
    if (save == null) return ""
    val device = save.device?.let {
        "${it.name}（${if (it.kind == DeviceKind.EMULATOR) "Emulator" else "実機"}・${it.serial}）"
    } ?: "未確定"
    val app = save.application
    val mode = when (app?.mode) {
        ApplicationMode.AUTO -> "自動"
        ApplicationMode.MANUAL -> "手動"
        null -> "選択未確定"
    }
    val history = save.applicationHistory.joinToString("\n") {
        "${it.packageName ?: "対象未確定"}: ${recordTime(it.fromNs)}〜${recordTime(it.toNs)}" +
            if (it.resolved) "" else "（帰属未確定）"
    }.ifEmpty { "記録なし" }
    return "固定モード: ${captureModeDescription(save.captureMode)}\n固定端末: $device\n固定アプリ: ${app?.packageName ?: app?.unresolvedReason ?: "未確定"}（$mode）\n対象アプリ履歴:\n$history"
}

internal fun captureModeDescription(mode: CaptureMode): String = when (mode) {
    CaptureMode.VIDEO_AND_LOGS -> "動画＋ログ"
    CaptureMode.LOG_ONLY -> "ログ専用"
}

internal fun saveDescription(save: SaveSnapshot?): String = (when (save?.phase) {
    SavePhase.WRITING -> "保存: 保存中…（取得は継続しています）"
    SavePhase.FAILED -> "保存: 保存失敗"
    SavePhase.CANCELLED -> "保存: 公開前に取消し"
    SavePhase.COMPLETED -> "保存: 完了" + if (save.missingKinds.isNotEmpty()) " / 記録の不足・未確認: ${save.missingKinds.map(::streamName).joinToString("、")}" else ""
    else -> "保存: 待機"
}) + (if (save?.autoEventId != null) "（自動保存）" else "") +
    if (save != null && save.phase != SavePhase.IDLE) " / ${captureModeDescription(save.captureMode)}" else ""

internal fun autoSaveDescription(snapshot: ReplaySnapshot?): String {
    if (snapshot == null) return "クラッシュ自動保存: 設定を確認中…"
    val auto = snapshot.autoSave
    return buildString {
        append("クラッシュ自動保存: ${if (snapshot.settings.autoSaveOnCrash) "設定ON" else "設定OFF"} / ${auto.reason.orEmpty()}")
        append("\n受付可能: 残り${auto.admissionsRemaining}回 / 保存しなかった報告: ${auto.notSavedCount}件")
        auto.lastOutcome?.let { append("\n直近: ${it.reason.description}") }
        auto.warning?.let { append("\n要確認: $it") }
        snapshot.lastSave?.takeIf { it.autoEventId != null }?.let {
            append("\n保存要求: ${it.requestId} / 試行${it.attempt} / ${saveDescription(it)}")
            append("\n保存先: ${it.directory}")
            it.retainedPath?.let { path -> append("\n保全した出力: $path") }
        }
        append("\nnative/ANRは対応外。基準はplatformの報告時刻で、死亡時刻ではありません。")
    }
}

internal fun videoTailDescription(save: SaveSnapshot?): String {
    val tail = save?.videoTail ?: return ""
    if (save.phase == SavePhase.IDLE) return ""
    val range = "記録時刻: ${recordTime(tail.fromNs)}〜${recordTime(tail.toNs)}"
    if (save.phase != SavePhase.COMPLETED) return "固定対象の動画末尾: 新frame未確認（$range）"
    val display = if (tail.displayHeld) "前の画像を表示・新frame未確認" else "新frame未確認"
    return "動画末尾: $display（$range）"
}

internal fun cleanupDescription(count: Int): String = if (count <= 0) "" else
    "前回の端末処理の片付け待ち: ${count}件。対象端末を接続すると再確認します。"

internal fun recordTime(nanos: Long?): String = nanos?.let {
    String.format(Locale.ROOT, "%.3f秒", it / 1_000_000_000.0)
} ?: "未確定"

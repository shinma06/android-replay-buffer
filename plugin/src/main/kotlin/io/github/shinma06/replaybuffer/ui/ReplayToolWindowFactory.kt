package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.Disposable
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.ui.componentsList.components.ScrollablePanel
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.ui.TitledSeparator
import com.intellij.ui.components.JBCheckBox
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.JBTextArea
import com.intellij.ui.content.ContentFactory
import com.intellij.util.ui.FormBuilder
import com.intellij.util.ui.JBUI
import com.intellij.util.ui.WrapLayout
import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.core.StreamSnapshot
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.StreamState
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.ide.streamName
import io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurable
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import java.awt.BorderLayout
import java.awt.Dimension
import java.awt.FlowLayout
import java.util.Locale
import javax.swing.JButton
import javax.swing.JPanel
import javax.swing.text.DefaultCaret
import javax.swing.text.View
import kotlin.math.ceil

class ReplayToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = ReplayToolWindowPanel(project)
        val content = ContentFactory.getInstance().createContent(panel, "", false)
        Disposer.register(content, panel)
        content.setPreferredFocusableComponent(if (panel.enabled.isEnabled) panel.enabled else panel.settingsButton)
        toolWindow.contentManager.addContent(content)
    }
}

private class ReplayToolWindowPanel(private val project: Project) : JPanel(BorderLayout()), Disposable {
    private val service = project.getService(ReplayProjectService::class.java)
    private val store = project.getService(ReplaySettingsStore::class.java)
    private var disposed = false
    private var shownSave: SaveSnapshot? = null
    val enabled = JBCheckBox("取得を有効にする").apply {
        toolTipText = "ON/OFFはすぐ反映します。OFFにすると未保存の記録と再試行待ちの対象を破棄します。"
        addActionListener { if (alive) service.setEnabled(isSelected) }
    }
    val settingsButton = JButton("設定…").apply {
        toolTipText = "取得OFFでも編集できます。変更は設定画面の「適用」または「OK」で反映します。"
        addActionListener { if (alive) ShowSettingsUtil.getInstance().showSettingsDialog(project, ReplaySettingsConfigurable::class.java) }
    }
    private val screenshotButton = JButton("スクリーンショット").apply {
        accessibleContext.accessibleName = "現在画面のスクリーンショットを保存"
        addActionListener { if (alive) service.screenshot() }
    }
    private val screenshotReason = wrappingLabel(name = "スクリーンショットの対象・保存先と操作条件")
    private val device = wrappingLabel(name = "取得対象の端末")
    private val application = wrappingLabel(name = "取得対象のアプリ")
    private val sdk = wrappingLabel(name = "Android SDKの接続情報")
    private val state = wrappingLabel(name = "取得状態と取得モード")
    private val video = wrappingLabel(name = "動画の取得状態")
    private val deviceLog = wrappingLabel(name = "端末ログの取得状態")
    private val appLog = wrappingLabel(name = "アプリログの取得状態")
    private val window = wrappingLabel(name = "保持時間と現在の記録区間")
    private val sequence = wrappingLabel(name = "記録セッション")
    private val destination = wrappingLabel(name = "動画・ログの保存先")
    private val message = wrappingLabel(name = "設定・操作の結果と取得エラー")
    private val cleanup = wrappingLabel(name = "端末処理の片付け待ち")
    private val saveState = wrappingLabel(name = "直前保存の結果")
    private val saveOutput = wrappingLabel(name = "完成または保全した出力の場所")
    private val autoSave = readOnlyDetails("クラッシュ自動保存の適用状態と直近の結果", 3)
    private val videoTail = wrappingLabel(name = "保存対象の動画末尾の確認状態")
    private val saveReason = wrappingLabel(name = "直前保存の操作条件")
    private val failure = wrappingLabel(name = "保存失敗の理由")
    private val frozenRequest = wrappingLabel(name = "再試行する固定保存区間と不足・未確認")
    private val frozenTarget = readOnlyDetails("固定した保存対象の端末とアプリ履歴", 4)
    private val gapDetails = readOnlyDetails("現在の保存窓の不足・未確認区間", 4)
    private val gapScroll = JBScrollPane(gapDetails)
    private val saveButton = JButton().apply { addActionListener { if (alive) service.save() } }
    private val openButton = JButton("フォルダを開く").apply {
        addActionListener { if (alive) shownSave?.takeIf { it.phase == SavePhase.COMPLETED }?.directory?.let(service::openDirectory) }
    }
    private val retryButton = JButton("同じ対象を再試行").apply {
        addActionListener { if (alive) failedId()?.let(service::retry) }
    }
    private val retryElsewhere = JButton("別の保存先で再試行…").apply {
        addActionListener { if (alive) failedId()?.let(::chooseRetryDirectory) }
    }
    private val discardButton = JButton("未保存の対象を破棄").apply {
        addActionListener { if (alive) failedId()?.let(service::discard) }
    }
    private val failureCard: JPanel
    private val alive: Boolean get() = !disposed && !project.isDisposed

    init {
        failureCard = FormBuilder.createFormBuilder()
            .addComponent(failure)
            .addComponent(frozenRequest)
            .addComponent(wrappingLabel("再試行はこの固定した区間を使います。破棄しても取得は続きます。"))
            .addComponent(row(retryButton, retryElsewhere, discardButton))
            .addComponent(JBScrollPane(frozenTarget))
            .addComponent(wrappingLabel("取得OFFまたはproject終了で、この未保存の対象を破棄します。"))
            .panel
        val form = FormBuilder.createFormBuilder()
            .addComponent(row(enabled, settingsButton))
            .addComponent(wrappingLabel("ON/OFFはすぐ反映します。OFFで未保存の記録と再試行待ちの対象を破棄します。"))
            .addComponent(state).addComponent(message).addComponent(device).addComponent(application).addComponent(window)
            .addComponent(row(saveButton, screenshotButton))
            .addComponent(saveReason).addComponent(screenshotReason)
            .addComponent(TitledSeparator("保存結果"))
            .addComponent(saveState).addComponent(videoTail).addComponent(saveOutput).addComponent(openButton)
            .addComponent(failureCard)
            .addComponent(TitledSeparator("記録の状態"))
            .addComponent(video).addComponent(deviceLog).addComponent(appLog).addComponent(gapScroll)
            .addComponent(TitledSeparator("保存先・自動保存"))
            .addComponent(destination).addComponent(JBScrollPane(autoSave))
            .addComponent(TitledSeparator("接続情報"))
            .addComponent(sdk).addComponent(sequence).addComponent(cleanup)
            .addComponentFillVertically(JPanel(), 0).panel.apply { border = JBUI.Borders.empty(8) }
        val content = ScrollablePanel(BorderLayout()).apply { add(form, BorderLayout.CENTER) }
        add(JBScrollPane(content).apply { border = null }, BorderLayout.CENTER)
        val refresh = Runnable {
            if (alive) ToolWindowManager.getInstance(project).invokeLater { if (alive) render() }
        }
        val connection = project.messageBus.connect(this)
        connection.subscribe(ReplayProjectService.ENVIRONMENT_CHANGED, refresh)
        connection.subscribe(ReplaySettingsStore.CHANGED, refresh)
        render()
    }

    private fun failedId(): String? = shownSave?.takeIf { it.phase == SavePhase.FAILED }?.requestId

    private fun chooseRetryDirectory(id: String) {
        FileChooser.chooseFile(FileChooserDescriptorFactory.createSingleFolderDescriptor(), project, null) { selected ->
            if (!alive) return@chooseFile
            val path = selected.toNioPath()
            val answer = Messages.showYesNoDialog(project,
                "この保存先で、固定した保存対象を再試行します。\n$path\n\n通常設定の保存先は変更しません。",
                "保存先を変更して再試行", "適用して再試行", "キャンセル", Messages.getQuestionIcon())
            if (alive && answer == Messages.YES) service.retryAtDirectory(id, path)
        }
    }

    private fun render() {
        val view = service.captureView
        val snapshot = view.snapshot
        val busy = service.resolving || view.pending > 0
        val screenshotDisabled = service.screenshotDisabledReason
        screenshotButton.isEnabled = screenshotDisabled == null
        screenshotButton.text = if (service.screenshotBusy) "スクショを保存中…" else "スクリーンショット"
        screenshotButton.toolTipText = screenshotDisabled ?: "録画対象の端末で選択アプリを表示してください。取得OFFでは接続済みの1台から現在の画面全体を保存します。"
        screenshotReason.text = screenshotDisabled ?: "スクショ対象: ${service.environment?.application?.packageName} / 保存先: ${if (store.settings().screenshotUsesLogcatDestination) "Logcatのスクショ保存先（保存時の最新設定）" else store.settings().screenshotDirectory}\n" +
            "撮影中は画面を切り替えないでください。通知・キーボードも写り、保護領域は空白になる場合があります。"
        enabled.isSelected = store.enabled
        enabled.isEnabled = store.enabled || !busy
        device.text = snapshot?.device?.let {
            "端末: ${it.name}（${if (it.kind == DeviceKind.EMULATOR) "Emulator" else "実機"}・${if (it.connected) "接続済み" else "切断中"}）"
        } ?: "端末: 未接続"
        val app = snapshot?.settings?.application
        application.text = "アプリ: ${app?.packageName ?: app?.unresolvedReason ?: "確認中…"}（${if (app?.mode == ApplicationMode.MANUAL) "手動" else "自動"}）"
        sdk.text = "Android SDK: ${service.environment?.adb ?: service.environment?.adbReason ?: "確認中…"}"
        state.text = snapshot?.captureDescription() ?: if (view.initializationError != null) "取得を初期化できません" else "取得を初期化中…"
        state.text += " / 取得モード: ${captureModeDescription(snapshot?.settings?.captureMode ?: store.settings().captureMode)}"
        if (snapshot != null && snapshot.settings.captureMode != store.settings().captureMode)
            state.text += "（設定の反映待ち: ${captureModeDescription(store.settings().captureMode)}）"
        video.text = streamDescription("動画", snapshot?.video)
        deviceLog.text = streamDescription("端末ログ", snapshot?.deviceLog)
        appLog.text = streamDescription("アプリログ", snapshot?.appLog)
        val seconds = snapshot?.settings?.replaySeconds ?: store.settings().retentionSeconds
        window.text = "保持時間: ${seconds}秒 / 記録区間: ${recordTime(snapshot?.windowStartNs)}〜${recordTime(snapshot?.windowEndNs)}" +
            if (snapshot?.frozen == true) "（切断前で固定・無効化まで保持）" else "（現在の窓）"
        sequence.text = "記録セッション: ${snapshot?.sequenceId ?: "未開始"}"
        destination.text = "保存先: ${snapshot?.settings?.saveDirectory ?: "未指定（設定で指定してください）"}"
        val autoText = autoSaveDescription(snapshot) + if (snapshot != null &&
            snapshot.settings.autoSaveOnCrash != store.settings().autoSaveOnCrash) "\n自動保存設定の反映待ち" else ""
        if (autoSave.text != autoText) autoSave.text = autoText
        message.text = if (busy) "設定・操作を反映中…（新規保存は完了まで待ってください）" else view.message ?: view.initializationError ?: snapshot?.error ?: " "
        message.isVisible = message.text.isNotBlank()
        cleanup.text = cleanupDescription(snapshot?.cleanupPendingCount ?: 0)
        cleanup.isVisible = cleanup.text.isNotEmpty()
        saveButton.text = "直前${seconds}秒を保存"
        saveButton.isEnabled = !busy && snapshot?.canSave == true
        saveReason.text = if (busy) "設定・操作を反映中です。" else snapshot?.saveDisabledReason ?: " "
        saveReason.isVisible = saveReason.text.isNotBlank()
        saveButton.toolTipText = saveReason.text
        shownSave = snapshot?.save
        val saved = snapshot?.save
        saveState.text = saveDescription(saved)
        saveOutput.text = when (saved?.phase) {
            SavePhase.COMPLETED -> saved.directory?.let { "保存したフォルダ: $it" }.orEmpty()
            SavePhase.FAILED -> saved.retainedPath?.let { "保全した出力: $it" }.orEmpty()
            else -> ""
        }
        saveOutput.isVisible = saveOutput.text.isNotEmpty()
        videoTail.text = videoTailDescription(saved)
        videoTail.isVisible = videoTail.text.isNotEmpty()
        openButton.isVisible = saved?.phase == SavePhase.COMPLETED && saved.directory != null
        failureCard.isVisible = saved?.phase == SavePhase.FAILED
        failure.text = "保存失敗の理由: ${saved?.error ?: "保存先を確認してください。"}"
        frozenRequest.text = "固定対象: ${recordTime(saved?.windowStartNs)}〜${recordTime(saved?.windowEndNs)} / ${saved?.replaySeconds ?: seconds}秒" +
            " / セッション: ${saved?.sequenceId ?: "未確定"}" +
            " / 記録の不足・未確認: ${saved?.missingKinds?.map(::streamName)?.joinToString("、")?.ifEmpty { "なし" } ?: "なし"}"
        val targetText = frozenTargetDescription(saved)
        if (frozenTarget.text != targetText) frozenTarget.text = targetText
        val gapText = currentGapDescription(snapshot)
        if (gapDetails.text != gapText) gapDetails.text = gapText
        gapScroll.isVisible = gapDetails.text.isNotEmpty()
        retryButton.isEnabled = !busy && saved?.phase == SavePhase.FAILED
        retryElsewhere.isEnabled = retryButton.isEnabled
        discardButton.isEnabled = retryButton.isEnabled
        revalidate()
        repaint()
    }

    override fun dispose() { disposed = true }
}

private fun row(vararg components: java.awt.Component): JPanel = JPanel(WrapLayout(FlowLayout.LEFT).apply {
    hgap = JBUI.scale(4)
    vgap = JBUI.scale(2)
}).apply {
    components.forEach(::add)
}

internal fun wrappingLabel(text: String = "", name: String = text): JBTextArea = object : JBTextArea(text) {
    override fun updateUI() {
        super.updateUI()
        (caret as DefaultCaret).updatePolicy = DefaultCaret.NEVER_UPDATE
        font = javax.swing.UIManager.getFont("Label.font")
    }

    override fun getPreferredSize(): Dimension {
        val size = super.getPreferredSize().apply { width = 0 }
        val container = parent ?: return size
        val textUi = getUI()
        val view = textUi.getRootView(this)
        // Match the existing settings guidance sizing, including insets and caret space.
        val reservedWidth = textUi.getPreferredSize(this).width - view.getPreferredSpan(View.X_AXIS).toInt()
        val contentWidth = container.width - container.insets.left - container.insets.right - reservedWidth
        if (contentWidth > 0) {
            view.setSize(contentWidth.toFloat(), Float.MAX_VALUE)
            size.height = ceil(view.getPreferredSpan(View.Y_AXIS).toDouble()).toInt() + insets.top + insets.bottom
        }
        return size
    }
}.apply {
    isEditable = false
    isFocusable = false
    isOpaque = false
    lineWrap = true
    wrapStyleWord = true
    accessibleContext.accessibleName = name
}

private fun readOnlyDetails(name: String, visibleRows: Int): JBTextArea = JBTextArea(visibleRows, 30).apply {
    isEditable = false
    lineWrap = true
    wrapStyleWord = true
    font = javax.swing.UIManager.getFont("Label.font")
    accessibleContext.accessibleName = name
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

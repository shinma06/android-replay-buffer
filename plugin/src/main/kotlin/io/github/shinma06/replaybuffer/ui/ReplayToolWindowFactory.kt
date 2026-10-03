package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.Disposable
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
import com.intellij.ui.components.JBCheckBox
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.JBTextArea
import com.intellij.ui.content.ContentFactory
import com.intellij.util.ui.FormBuilder
import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.core.StreamSnapshot
import io.github.shinma06.replaybuffer.core.StreamState
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.ide.streamName
import io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurable
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import java.awt.BorderLayout
import java.awt.FlowLayout
import java.util.Locale
import javax.swing.JButton
import javax.swing.JPanel

class ReplayToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = ReplayToolWindowPanel(project)
        val content = ContentFactory.getInstance().createContent(panel, "", false)
        Disposer.register(content, panel)
        content.setPreferredFocusableComponent(panel.settingsButton)
        toolWindow.contentManager.addContent(content)
    }
}

private class ReplayToolWindowPanel(private val project: Project) : JPanel(BorderLayout()), Disposable {
    private val service = project.getService(ReplayProjectService::class.java)
    private val store = project.getService(ReplaySettingsStore::class.java)
    private var disposed = false
    private var shownSave: SaveSnapshot? = null
    private val enabled = JBCheckBox("取得を有効にする").apply {
        toolTipText = "無効にすると未保存のデータと再試行待ちの内容を破棄します。"
        addActionListener { if (alive) service.setEnabled(isSelected) }
    }
    val settingsButton = JButton("設定…").apply {
        toolTipText = "取得が無効でも設定を編集できます。"
        addActionListener { if (alive) ShowSettingsUtil.getInstance().showSettingsDialog(project, ReplaySettingsConfigurable::class.java) }
    }
    private val device = JBLabel()
    private val application = JBLabel()
    private val sdk = JBLabel()
    private val state = JBLabel()
    private val video = JBLabel()
    private val deviceLog = JBLabel()
    private val appLog = JBLabel()
    private val window = JBLabel()
    private val sequence = JBLabel()
    private val destination = JBLabel()
    private val message = JBLabel()
    private val saveState = JBLabel()
    private val saveReason = JBLabel()
    private val failure = JBLabel()
    private val frozenRequest = JBLabel()
    private val frozenTarget = readOnlyDetails("固定した保存対象の端末とアプリ履歴", 4)
    private val gapDetails = readOnlyDetails("現在の保存窓の欠落", 4)
    private val gapScroll = JBScrollPane(gapDetails)
    private val saveButton = JButton().apply { addActionListener { if (alive) service.save() } }
    private val openButton = JButton("フォルダを開く").apply {
        addActionListener { if (alive) shownSave?.takeIf { it.phase == SavePhase.COMPLETED }?.directory?.let(service::openDirectory) }
    }
    private val retryButton = JButton("再試行").apply {
        addActionListener { if (alive) failedId()?.let(service::retry) }
    }
    private val retryElsewhere = JButton("保存先を変更して再試行…").apply {
        addActionListener { if (alive) failedId()?.let(::chooseRetryDirectory) }
    }
    private val discardButton = JButton("破棄").apply {
        addActionListener { if (alive) failedId()?.let(service::discard) }
    }
    private val failureCard: JPanel
    private val alive: Boolean get() = !disposed && !project.isDisposed

    init {
        failureCard = FormBuilder.createFormBuilder()
            .addComponent(failure)
            .addComponent(frozenRequest)
            .addComponent(JBScrollPane(frozenTarget))
            .addComponent(JBLabel("無効化またはproject終了で、この未保存の対象を破棄します。"))
            .addComponent(row(retryButton, retryElsewhere, discardButton))
            .panel
        val form = FormBuilder.createFormBuilder()
            .addComponent(row(enabled, settingsButton))
            .addComponent(JBLabel("無効にすると未保存のデータと再試行待ちの内容を破棄します。"))
            .addComponent(device).addComponent(application).addComponent(sdk).addComponent(state)
            .addComponent(video).addComponent(deviceLog).addComponent(appLog)
            .addComponent(window).addComponent(sequence).addComponent(destination).addComponent(message)
            .addComponent(saveButton).addComponent(saveReason).addComponent(saveState)
            .addComponent(openButton).addComponent(failureCard)
            .addComponent(gapScroll)
            .addComponentFillVertically(JPanel(), 0).panel
        add(JBScrollPane(form).apply { border = null }, BorderLayout.CENTER)
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
        enabled.isSelected = store.enabled
        device.text = snapshot?.device?.let {
            "端末: ${it.name}（${if (it.kind == DeviceKind.EMULATOR) "Emulator" else "実機"}・${if (it.connected) "接続済み" else "切断中"}）"
        } ?: "端末: 未接続"
        val app = snapshot?.settings?.application
        application.text = "アプリ: ${app?.packageName ?: app?.unresolvedReason ?: "確認中…"}（${if (app?.mode == ApplicationMode.MANUAL) "手動" else "自動"}）"
        sdk.text = "Android SDK: ${service.environment?.adb ?: service.environment?.adbReason ?: "確認中…"}"
        state.text = snapshot?.captureDescription() ?: "取得を初期化中…"
        video.text = streamDescription("動画", snapshot?.video)
        deviceLog.text = streamDescription("端末ログ", snapshot?.deviceLog)
        appLog.text = streamDescription("アプリログ", snapshot?.appLog)
        val seconds = snapshot?.settings?.replaySeconds ?: store.settings().retentionSeconds
        window.text = "保持時間: ${seconds}秒 / 記録区間: ${recordTime(snapshot?.windowStartNs)}〜${recordTime(snapshot?.windowEndNs)}" +
            if (snapshot?.frozen == true) "（切断前で固定・無効化まで保持）" else "（現在の窓）"
        sequence.text = "記録セッション: ${snapshot?.sequenceId ?: "未開始"}"
        destination.text = "保存先: ${snapshot?.settings?.saveDirectory ?: "未指定（設定で指定してください）"}"
        message.text = if (busy) "設定・操作を反映中…（新規保存は完了まで待ってください）" else view.message ?: snapshot?.error ?: " "
        saveButton.text = "直前${seconds}秒を保存"
        saveButton.isEnabled = !busy && snapshot?.canSave == true
        saveReason.text = if (busy) "設定・操作を反映中です。" else snapshot?.saveDisabledReason ?: " "
        saveButton.toolTipText = saveReason.text
        shownSave = snapshot?.save
        val saved = snapshot?.save
        saveState.text = when (saved?.phase) {
            SavePhase.WRITING -> "保存: 保存中…（取得は継続しています）"
            SavePhase.FAILED -> "保存: 保存失敗（固定した対象を再試行できます）"
            SavePhase.COMPLETED -> "保存: 完了" + if (saved.missingKinds.isNotEmpty()) " / 不足・欠落: ${saved.missingKinds.map(::streamName).joinToString("、")}" else ""
            else -> "保存: 待機"
        }
        openButton.isVisible = saved?.phase == SavePhase.COMPLETED && saved.directory != null
        failureCard.isVisible = saved?.phase == SavePhase.FAILED
        failure.text = "保存できません: ${saved?.error ?: "保存先を確認してください。"}"
        frozenRequest.text = "固定対象: ${recordTime(saved?.windowStartNs)}〜${recordTime(saved?.windowEndNs)} / ${saved?.replaySeconds ?: seconds}秒" +
            " / セッション: ${saved?.sequenceId ?: "未確定"}" +
            " / 不足・欠落: ${saved?.missingKinds?.map(::streamName)?.joinToString("、")?.ifEmpty { "なし" } ?: "なし"}"
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

private fun row(vararg components: java.awt.Component): JPanel = JPanel(FlowLayout(FlowLayout.LEFT)).apply {
    components.forEach(::add)
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
    }
    return "$label: $state / ${String.format(Locale.ROOT, "%.1f", stream.availableSeconds)}秒分" +
        (stream.reason?.let { " / $it" } ?: "") +
        if (stream.gaps.isEmpty()) "" else " / 現在窓に欠落${stream.gaps.size}件（下の欠落詳細）"
}

internal fun currentGapDescription(snapshot: ReplaySnapshot?): String {
    if (snapshot == null) return ""
    val gaps = (snapshot.video.gaps + snapshot.deviceLog.gaps + snapshot.appLog.gaps).distinct()
    if (gaps.isEmpty()) return ""
    return "現在窓内の欠落（記録時刻）:\n" + gaps.joinToString("\n") { gap ->
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
    return "固定端末: $device\n固定アプリ: ${app?.packageName ?: app?.unresolvedReason ?: "未確定"}（$mode）\n対象アプリ履歴:\n$history"
}

internal fun recordTime(nanos: Long?): String = nanos?.let {
    String.format(Locale.ROOT, "%.3f秒", it / 1_000_000_000.0)
} ?: "未確定"

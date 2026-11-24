package io.github.shinma06.arb.ide

import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.DumbAwareToggleAction
import com.intellij.openapi.project.Project
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.SaveTriggerParams
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.ui.persistCaptureEnabled
import javax.swing.Icon
import javax.swing.SwingUtilities

/** Pure presentation state for Running Devices & Tools Menu actions (headless-testable). */
data class ArbActionPresentation(
    val text: String,
    val description: String,
    val icon: Icon,
    val enabled: Boolean,
    val selected: Boolean = false,
)

object ArbToolbarPresentationState {
    fun toggleCapture(snapshot: ArbToolbarDaemonSnapshot): ArbActionPresentation {
        val active = snapshot.captureActive
        val armed = snapshot.captureEnabled
        val isOn = active || armed
        val icon = if (isOn) AllIcons.Actions.Suspend else AllIcons.Actions.Restart
        if (!snapshot.probed) {
            return ArbActionPresentation(
                text = "ARB: キャプチャを開始",
                description = "arbd に接続してライブキャプチャを開始します",
                icon = AllIcons.Actions.Restart,
                enabled = true,
                selected = false,
            )
        }
        if (!snapshot.daemonConnected) {
            return ArbActionPresentation(
                text = "ARB: キャプチャを開始",
                description = "arbd 未接続（クリックで接続・自動起動を試行します）",
                icon = AllIcons.Actions.Restart,
                enabled = true,
                selected = false,
            )
        }
        if (active && snapshot.captureConnected) {
            val serialSuffix = snapshot.selectedDeviceSerial?.let { " ($it)" }.orEmpty()
            return ArbActionPresentation(
                text = "ARB: キャプチャを停止 (ON・REC)",
                description = "ON・REC（取得中）$serialSuffix — クリックでキャプチャを OFF にします",
                icon = AllIcons.Actions.Suspend,
                enabled = true,
                selected = true,
            )
        }
        if (isOn) {
            val detail = snapshot.disabledDeviceHint?.let { " · $it" }.orEmpty()
            return ArbActionPresentation(
                text = "ARB: キャプチャを停止 (ON・待機中)",
                description = "ON・待機中（端末接続で自動開始）$detail — クリックでキャプチャを OFF にします",
                icon = AllIcons.Actions.Suspend,
                enabled = true,
                selected = true,
            )
        }
        val canStartNow = snapshot.hasOnlineDevice && !snapshot.selectedDeviceSerial.isNullOrBlank()
        val reason = if (canStartNow) {
            "OFF（停止中） — ライブキャプチャを開始します (${snapshot.selectedDeviceSerial})"
        } else {
            val hint = snapshot.disabledDeviceHint?.let { " · $it" }.orEmpty()
            "OFF（停止中） — クリックで ON・待機中（端末接続で自動開始）にします$hint"
        }
        return ArbActionPresentation(
            text = "ARB: キャプチャを開始 (OFF)",
            description = reason,
            icon = icon,
            enabled = true,
            selected = false,
        )
    }

    fun quickSave(snapshot: ArbToolbarDaemonSnapshot, windowSecs: Int?): ArbActionPresentation {
        val text = if (windowSecs != null) {
            "ARB: 直前 ${ArbFormatters.formatSeconds(windowSecs.toLong())} を保存"
        } else {
            "ARB: 直前の記録を保存"
        }
        if (!snapshot.probed || !snapshot.daemonConnected) {
            return ArbActionPresentation(
                text = text,
                description = "arbd 経由で設定秒数分の画面・ログ・入力を保存します",
                icon = AllIcons.Actions.MenuSaveall,
                enabled = true,
            )
        }
        val canSave = snapshot.captureActive || snapshot.hasBufferedData
        val description = if (canSave) {
            "設定された秒数と既定アクションに従って直前の記録を保存します"
        } else {
            "保存可能なバッファがありません（先にキャプチャを開始してください）"
        }
        return ArbActionPresentation(
            text = text,
            description = description,
            icon = AllIcons.Actions.MenuSaveall,
            enabled = canSave,
        )
    }

    fun screenshot(snapshot: ArbToolbarDaemonSnapshot): ArbActionPresentation {
        val text = "ARB: スクリーンショットを保存"
        if (!snapshot.probed || !snapshot.daemonConnected) {
            return ArbActionPresentation(
                text = text,
                description = "対象端末の現在画面を PNG として即時保存します",
                icon = AllIcons.FileTypes.Image,
                enabled = true,
            )
        }
        val canCapture = snapshot.captureActive || (snapshot.hasOnlineDevice && !snapshot.selectedDeviceSerial.isNullOrBlank())
        val description = if (canCapture) {
            val serialSuffix = snapshot.selectedDeviceSerial?.let { " ($it)" }.orEmpty()
            "対象端末の現在画面を PNG として即時保存します$serialSuffix"
        } else {
            snapshot.disabledDeviceHint ?: "端末未検出（USB / エミュレータを接続してください）"
        }
        return ArbActionPresentation(
            text = text,
            description = description,
            icon = AllIcons.FileTypes.Image,
            enabled = canCapture,
        )
    }

    fun saveLogsOnly(snapshot: ArbToolbarDaemonSnapshot, windowSecs: Int?): ArbActionPresentation {
        val text = if (windowSecs != null) {
            "ARB: 直前 ${ArbFormatters.formatSeconds(windowSecs.toLong())} のログのみ保存"
        } else {
            "ARB: 直前のログのみ保存"
        }
        if (!snapshot.probed || !snapshot.daemonConnected) {
            return ArbActionPresentation(
                text = text,
                description = "映像キャプチャを中断せず直前のログのみを保存します",
                icon = AllIcons.FileTypes.Text,
                enabled = true,
            )
        }
        val canSave = snapshot.captureActive || snapshot.hasBufferedData
        return ArbActionPresentation(
            text = text,
            description = if (canSave) {
                "映像キャプチャを中断せず直前のログのみを保存します"
            } else {
                "保存可能なログバッファがありません（先にキャプチャを開始してください）"
            },
            icon = AllIcons.FileTypes.Text,
            enabled = canSave,
        )
    }
}

private fun executeConfiguredSave(project: Project?) {
    ApplicationManager.getApplication().executeOnPooledThread {
        val service = ArbdDaemonService.getInstance()
        try {
            val config = runCatching {
                service.withClient { it.configGet().config }.also(service::updateCachedConfig)
            }.getOrDefault(service.cachedConfig)
            val windowSecs = config.saveWindowSecs.toLong()
            val res = service.withClient {
                it.saveTrigger(SaveTriggerParams(durationSec = windowSecs))
            }
            service.notifyCatalogChanged()
            val savedId = res.bundleId
            if (savedId != null) {
                val openInStudio = config.ideDefaultSaveAction == IdeDefaultSaveAction.SAVE_AND_OPEN &&
                    project != null && !project.isDisposed
                if (openInStudio) {
                    SwingUtilities.invokeLater {
                        if (!project.isDisposed) {
                            val vf = ArbBundleVirtualFile(savedId, savedId, isLegacyHint = false)
                            FileEditorManager.getInstance(project).openFile(vf, true)
                        }
                    }
                }
                ArbNotifications.notifyReplaySaved(
                    project = project,
                    daemonService = service,
                    bundleId = savedId,
                    bundlePath = res.bundlePath,
                    windowSecs = windowSecs,
                    openedInStudio = openInStudio,
                    warning = res.warning,
                )
            } else {
                ArbNotifications.notifyWarning(
                    project = project,
                    message = "保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細は ARB パネルを確認してください"})",
                    includeOpenPanel = true,
                )
            }
        } catch (t: Throwable) {
            ArbNotifications.notifyActionFailed(project, "直前の記録の保存", t.message)
        }
    }
}

private fun executeSetCaptureState(project: Project?, desiredActive: Boolean) {
    ApplicationManager.getApplication().executeOnPooledThread {
        val service = ArbdDaemonService.getInstance()
        try {
            val status = service.withClient { it.systemStatus() }
            if (!desiredActive) {
                persistCaptureEnabled(service, enabled = false)
                if (status.capture.active) {
                    service.withClient { it.captureStop() }
                }
                service.updateCachedCaptureState(active = false, enabled = false)
                service.notifyCatalogChanged()
                ArbNotifications.notifyInfo(project, "キャプチャを OFF（停止中）にしました。")
            } else {
                // `null`: this arbd predates always-on capture, so only a direct start can turn it ON.
                val armed = persistCaptureEnabled(service, enabled = true) == true
                val snap = service.cachedToolbarSnapshot
                val serial = if (snap.hasOnlineDevice) snap.selectedDeviceSerial else null
                if (!armed && !status.capture.active && serial.isNullOrBlank()) {
                    ArbNotifications.notifyInfo(project, "端末未検出（USB / エミュレータを接続してください）")
                    service.updateCachedCaptureState(active = false, enabled = false)
                } else if (!status.capture.active && !serial.isNullOrBlank()) {
                    val attempt = runCatching {
                        service.withClient { it.captureStart(CaptureStartParams(serial = serial)) }
                    }
                    if (!armed) attempt.getOrThrow()
                    val started = attempt.getOrNull()
                    if (started != null) {
                        service.updateCachedCaptureState(active = true, enabled = armed)
                        service.notifyCatalogChanged()
                        ArbNotifications.notifyInfo(
                            project,
                            "キャプチャを ON・REC（取得中）にしました (端末: ${started.serial})。",
                        )
                    } else {
                        service.updateCachedCaptureState(active = false, enabled = true)
                        service.notifyCatalogChanged()
                        ArbNotifications.notifyInfo(project, "キャプチャを ON・待機中（端末接続で自動開始）にしました。")
                    }
                } else {
                    service.updateCachedCaptureState(active = status.capture.active, enabled = armed)
                    service.notifyCatalogChanged()
                    if (!status.capture.active) {
                        ArbNotifications.notifyInfo(project, "キャプチャを ON・待機中（端末接続で自動開始）にしました。")
                    }
                }
            }
        } catch (t: Throwable) {
            ArbNotifications.notifyActionFailed(project, "キャプチャ切替", t.message)
        }
    }
}

private fun executeScreenshot(project: Project?) {
    ApplicationManager.getApplication().executeOnPooledThread {
        val service = ArbdDaemonService.getInstance()
        try {
            val outputDir = runCatching { ArbScreenshotDestinationResolver.resolveOutputDir(project) }.getOrNull()
            val serial = service.cachedToolbarSnapshot.selectedDeviceSerial
            val res = service.withClient {
                it.saveScreenshot(SaveScreenshotParams(serial = serial, outputDir = outputDir))
            }
            ArbNotifications.notifyImageSaved(
                project = project,
                imagePath = res.path,
                width = res.width,
                height = res.height,
                kindLabel = "スクリーンショット",
            )
        } catch (t: Throwable) {
            ArbNotifications.notifyActionFailed(project, "スクリーンショットの保存", t.message)
        }
    }
}

private fun executeSaveLogsOnly(project: Project?) {
    ApplicationManager.getApplication().executeOnPooledThread {
        val service = ArbdDaemonService.getInstance()
        try {
            val config = runCatching {
                service.withClient { it.configGet().config }.also(service::updateCachedConfig)
            }.getOrDefault(service.cachedConfig)
            val windowSecs = config.saveWindowSecs.toLong()
            val res = service.withClient {
                it.saveLogs(SaveLogsParams(windowSecs = windowSecs))
            }
            service.notifyCatalogChanged()
            val savedId = res.bundleId
            if (savedId != null) {
                ArbNotifications.notifyLogsOnlySaved(
                    project = project,
                    daemonService = service,
                    bundleId = savedId,
                    bundlePath = res.bundlePath,
                    windowSecs = windowSecs,
                    warning = res.warning,
                )
            } else {
                ArbNotifications.notifyWarning(
                    project = project,
                    message = "ログ保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細は ARB パネルを確認してください"})",
                    includeOpenPanel = true,
                )
            }
        } catch (t: Throwable) {
            ArbNotifications.notifyActionFailed(project, "ログのみの保存", t.message)
        }
    }
}

private fun readToolbarSnapshotOrDefault(): ArbToolbarDaemonSnapshot =
    runCatching { ArbdDaemonService.getInstance().toolbarSnapshot() }.getOrDefault(ArbToolbarDaemonSnapshot())

private fun readWindowSecsOrNull(): Int? =
    runCatching { ArbdDaemonService.getInstance().cachedConfig.saveWindowSecs }.getOrNull()

class ArbQuickSaveAction :
    AnAction("ARB: 直前の記録を保存", "arbd 経由で設定秒数分の画面・ログ・入力を保存します", AllIcons.Actions.MenuSaveall),
    DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val state = ArbToolbarPresentationState.quickSave(readToolbarSnapshotOrDefault(), readWindowSecsOrNull())
        e.presentation.icon = state.icon
        e.presentation.text = state.text
        e.presentation.description = state.description
        e.presentation.isEnabled = state.enabled
    }

    override fun actionPerformed(e: AnActionEvent) {
        executeConfiguredSave(e.project)
    }
}

/**
 * Toggle action for Running Devices (both Embedded Emulator and Physical Device Mirroring via
 * `StreamingToolbarSecondary`) and the Tools menu. Extending [DumbAwareToggleAction] ensures the
 * toolbar button renders an unmistakable pressed/toggled state while capture is active.
 */
class ArbToggleCaptureAction :
    DumbAwareToggleAction("ARB: キャプチャ開始 / 停止", "arbd のライブキャプチャを開始または停止します", AllIcons.Actions.Restart) {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun isSelected(e: AnActionEvent): Boolean {
        val snap = readToolbarSnapshotOrDefault()
        return snap.captureEnabled || snap.captureActive
    }

    override fun setSelected(e: AnActionEvent, state: Boolean) {
        executeSetCaptureState(e.project, desiredActive = state)
    }

    override fun update(e: AnActionEvent) {
        super.update(e)
        val state = ArbToolbarPresentationState.toggleCapture(readToolbarSnapshotOrDefault())
        e.presentation.icon = state.icon
        e.presentation.text = state.text
        e.presentation.description = state.description
        e.presentation.isEnabled = state.enabled
    }
}

class ArbScreenshotAction :
    AnAction("ARB: スクリーンショットを保存", "対象端末の現在画面を PNG として即時保存します", AllIcons.FileTypes.Image),
    DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val state = ArbToolbarPresentationState.screenshot(readToolbarSnapshotOrDefault())
        e.presentation.icon = state.icon
        e.presentation.text = state.text
        e.presentation.description = state.description
        e.presentation.isEnabled = state.enabled
    }

    override fun actionPerformed(e: AnActionEvent) {
        executeScreenshot(e.project)
    }
}

class ArbLogcatSaveLogsAction :
    AnAction("ARB: 直前のログのみ保存", "映像キャプチャを中断せず直前のログのみを保存します", AllIcons.FileTypes.Text),
    DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val state = ArbToolbarPresentationState.saveLogsOnly(readToolbarSnapshotOrDefault(), readWindowSecsOrNull())
        e.presentation.icon = state.icon
        e.presentation.text = state.text
        e.presentation.description = state.description
        e.presentation.isEnabled = state.enabled
    }

    override fun actionPerformed(e: AnActionEvent) {
        executeSaveLogsOnly(e.project)
    }
}

class ArbLaunchTrayAction :
    AnAction(
        "ARB: メニューバー (arb-tray) を起動",
        "同梱の arb-tray をターミナル不要で起動します",
        AllIcons.Toolwindows.ToolWindowRun,
    ),
    DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project
        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                val bin = ArbdDaemonService.getInstance().launchBundledTray()
                ArbNotifications.notifyInfo(project, "メニューバー / トレイ (arb-tray) を起動しました: ${bin.fileName}")
            } catch (t: Throwable) {
                ArbNotifications.notifyWarning(project, "arb-tray の起動に失敗しました: ${t.message}")
            }
        }
    }
}

class ArbOpenBundleAction :
    AnAction("ARB: バンドルフォルダを開く...", "ARB バンドルまたは旧形式フォルダを統合スタジオで開きます", AllIcons.Nodes.Folder),
    DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val descriptor = FileChooserDescriptorFactory.createSingleFolderDescriptor().apply {
            title = "ARB / Legacy バンドルフォルダを選択"
        }
        val chosen = FileChooser.chooseFile(descriptor, project, null) ?: return
        val vf = ArbBundleVirtualFile(chosen.path, chosen.name)
        FileEditorManager.getInstance(project).openFile(vf, true)
    }
}

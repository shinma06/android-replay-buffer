package io.github.shinma06.arb.ide

import com.intellij.icons.AllIcons
import com.intellij.ide.actions.RevealFileAction
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.SaveTriggerParams
import io.github.shinma06.arb.state.ArbFormatters
import java.nio.file.Path
import javax.swing.SwingUtilities

private fun notifyArb(
    project: Project?,
    content: String,
    type: NotificationType = NotificationType.INFORMATION,
    revealPath: String? = null,
    openBundleId: String? = null,
) {
    val notification = NotificationGroupManager.getInstance()
        .getNotificationGroup("ARB v2")
        .createNotification("Android Replay Buffer (v2)", content, type)
    if (!revealPath.isNullOrBlank()) {
        notification.addAction(
            NotificationAction.createSimple("フォルダを表示") {
                runCatching { RevealFileAction.openFile(Path.of(revealPath)) }
            },
        )
    }
    if (project != null && !project.isDisposed && !openBundleId.isNullOrBlank()) {
        notification.addAction(
            NotificationAction.createSimple("スタジオで開く") {
                if (!project.isDisposed) {
                    val vf = ArbBundleVirtualFile(openBundleId, openBundleId, isLegacyHint = false)
                    FileEditorManager.getInstance(project).openFile(vf, true)
                }
            },
        )
    }
    notification.notify(project)
}

private fun executeConfiguredSave(project: Project?) {
    ApplicationManager.getApplication().executeOnPooledThread {
        try {
            val service = ArbdDaemonService.getInstance()
            val config = runCatching {
                service.withClient { it.configGet().config }.also(service::updateCachedConfig)
            }.getOrDefault(service.cachedConfig)
            val windowSecs = config.saveWindowSecs.toLong()
            val res = service.withClient {
                it.saveTrigger(SaveTriggerParams(durationSec = windowSecs))
            }
            val savedId = res.bundleId
            if (savedId != null) {
                if (config.ideDefaultSaveAction == IdeDefaultSaveAction.SAVE_AND_OPEN && project != null && !project.isDisposed) {
                    SwingUtilities.invokeLater {
                        if (!project.isDisposed) {
                            val vf = ArbBundleVirtualFile(savedId, savedId, isLegacyHint = false)
                            FileEditorManager.getInstance(project).openFile(vf, true)
                        }
                    }
                    notifyArb(
                        project = project,
                        content = "直前 ${ArbFormatters.formatSeconds(windowSecs)} を保存してスタジオで開きました: $savedId",
                        revealPath = res.bundlePath,
                    )
                } else {
                    notifyArb(
                        project = project,
                        content = "直前 ${ArbFormatters.formatSeconds(windowSecs)} を保存しました: $savedId",
                        revealPath = res.bundlePath,
                        openBundleId = savedId,
                    )
                }
            } else {
                notifyArb(
                    project = project,
                    content = "保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細は ARB ツールウィンドウを確認"})",
                    type = NotificationType.WARNING,
                )
            }
        } catch (t: Throwable) {
            notifyArb(project, "保存に失敗しました: ${t.message}", NotificationType.ERROR)
        }
    }
}

private fun executeToggleCapture(project: Project?) {
    ApplicationManager.getApplication().executeOnPooledThread {
        try {
            val service = ArbdDaemonService.getInstance()
            val status = service.withClient { it.systemStatus() }
            if (status.capture.active) {
                service.withClient { it.captureStop() }
                service.updateCachedCaptureActive(false)
                notifyArb(project, "ライブキャプチャを停止しました。")
            } else {
                val started = service.withClient { it.captureStart() }
                service.updateCachedCaptureActive(true)
                notifyArb(project, "ライブキャプチャを開始しました (端末: ${started.serial})。")
            }
        } catch (t: Throwable) {
            notifyArb(project, "キャプチャ切替に失敗しました: ${t.message}", NotificationType.ERROR)
        }
    }
}

private fun executeScreenshot(project: Project?) {
    ApplicationManager.getApplication().executeOnPooledThread {
        try {
            val service = ArbdDaemonService.getInstance()
            val res = service.withClient { it.saveScreenshot(SaveScreenshotParams()) }
            val fileName = runCatching { Path.of(res.path).fileName.toString() }.getOrDefault(res.path)
            notifyArb(
                project = project,
                content = "スクリーンショットを保存しました: $fileName (${res.width}x${res.height})",
                revealPath = res.path,
            )
        } catch (t: Throwable) {
            notifyArb(project, "スクリーンショットの保存に失敗しました: ${t.message}", NotificationType.ERROR)
        }
    }
}

private fun executeSaveLogsOnly(project: Project?) {
    ApplicationManager.getApplication().executeOnPooledThread {
        try {
            val service = ArbdDaemonService.getInstance()
            val config = runCatching {
                service.withClient { it.configGet().config }.also(service::updateCachedConfig)
            }.getOrDefault(service.cachedConfig)
            val windowSecs = config.saveWindowSecs.toLong()
            val res = service.withClient {
                it.saveLogs(SaveLogsParams(windowSecs = windowSecs))
            }
            val savedId = res.bundleId
            if (savedId != null) {
                notifyArb(
                    project = project,
                    content = "直前 ${ArbFormatters.formatSeconds(windowSecs)} のログのみ保存しました: $savedId",
                    revealPath = res.bundlePath,
                    openBundleId = savedId,
                )
            } else {
                notifyArb(
                    project = project,
                    content = "ログ保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細は ARB ツールウィンドウを確認"})",
                    type = NotificationType.WARNING,
                )
            }
        } catch (t: Throwable) {
            notifyArb(project, "ログのみの保存に失敗しました: ${t.message}", NotificationType.ERROR)
        }
    }
}

class ArbQuickSaveAction : AnAction("ARB v2: 直前の記録を保存", "arbd 経由で設定秒数分の画面・ログ・入力を保存します", AllIcons.Actions.MenuSaveall), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val windowSecs = runCatching { ArbdDaemonService.getInstance().cachedConfig.saveWindowSecs }.getOrNull()
        e.presentation.icon = AllIcons.Actions.MenuSaveall
        e.presentation.text = if (windowSecs != null) {
            "ARB v2: 直前 ${ArbFormatters.formatSeconds(windowSecs)} を保存"
        } else {
            "ARB v2: 直前の記録を保存"
        }
    }

    override fun actionPerformed(e: AnActionEvent) {
        executeConfiguredSave(e.project)
    }
}

class ArbToggleCaptureAction : AnAction("ARB v2: キャプチャ開始 / 停止", "arbd のライブキャプチャを開始または停止します", AllIcons.Actions.Restart), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val active = runCatching { ArbdDaemonService.getInstance().cachedCaptureActive }.getOrDefault(false)
        e.presentation.icon = if (active) AllIcons.Actions.Suspend else AllIcons.Actions.Restart
        e.presentation.text = if (active) "ARB v2: キャプチャを停止" else "ARB v2: キャプチャを開始"
    }

    override fun actionPerformed(e: AnActionEvent) {
        executeToggleCapture(e.project)
    }
}

class ArbScreenshotAction : AnAction("ARB v2: スクリーンショットを保存", "対象端末の現在画面を PNG として即時保存します", AllIcons.FileTypes.Image), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        e.presentation.icon = AllIcons.FileTypes.Image
        e.presentation.text = "ARB v2: スクリーンショットを保存"
    }

    override fun actionPerformed(e: AnActionEvent) {
        executeScreenshot(e.project)
    }
}

class ArbLogcatSaveLogsAction : AnAction("ARB v2: 直前のログのみ保存", "映像キャプチャを中断せず直前のログのみを保存します", AllIcons.FileTypes.Text), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val windowSecs = runCatching { ArbdDaemonService.getInstance().cachedConfig.saveWindowSecs }.getOrNull()
        e.presentation.icon = AllIcons.FileTypes.Text
        e.presentation.text = if (windowSecs != null) {
            "ARB v2: 直前 ${ArbFormatters.formatSeconds(windowSecs)} のログのみ保存"
        } else {
            "ARB v2: 直前のログのみ保存"
        }
    }

    override fun actionPerformed(e: AnActionEvent) {
        executeSaveLogsOnly(e.project)
    }
}

class ArbOpenBundleAction : AnAction("ARB v2: バンドルフォルダを開く...", "v2 バンドルまたは旧形式フォルダを統合スタジオで開きます", AllIcons.Nodes.Folder), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val descriptor = FileChooserDescriptorFactory.createSingleFolderDescriptor().apply {
            title = "ARB v2 / Legacy バンドルフォルダを選択"
        }
        val chosen = FileChooser.chooseFile(descriptor, project, null) ?: return
        val vf = ArbBundleVirtualFile(chosen.path, chosen.name)
        FileEditorManager.getInstance(project).openFile(vf, true)
    }
}

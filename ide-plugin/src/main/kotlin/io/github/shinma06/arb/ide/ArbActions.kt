package io.github.shinma06.arb.ide

import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.DumbAware
import io.github.shinma06.arb.model.SaveTriggerParams

private fun notifyArb(event: AnActionEvent, content: String, type: NotificationType = NotificationType.INFORMATION) {
    NotificationGroupManager.getInstance()
        .getNotificationGroup("ARB v2")
        .createNotification("Android Replay Buffer (v2)", content, type)
        .notify(event.project)
}

class ArbQuickSaveAction : AnAction(), DumbAware {
    override fun actionPerformed(e: AnActionEvent) {
        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                val service = ArbdDaemonService.getInstance()
                val windowSecs = runCatching { service.withClient { it.configGet().config.saveWindowSecs.toLong() } }
                    .getOrNull()
                val res = service.withClient {
                    it.saveTrigger(SaveTriggerParams(durationSec = windowSecs))
                }
                val msg = if (res.bundleId != null) {
                    "直前 ${windowSecs ?: 300}s を保存しました: ${res.bundleId}"
                } else {
                    "保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細は ARB ツールウィンドウを確認"})"
                }
                notifyArb(e, msg)
            } catch (t: Throwable) {
                notifyArb(e, "保存に失敗しました: ${t.message}", NotificationType.ERROR)
            }
        }
    }
}

class ArbToggleCaptureAction : AnAction(), DumbAware {
    override fun actionPerformed(e: AnActionEvent) {
        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                val service = ArbdDaemonService.getInstance()
                val status = service.withClient { it.systemStatus() }
                if (status.capture.active) {
                    service.withClient { it.captureStop() }
                    notifyArb(e, "ライブキャプチャを停止しました。")
                } else {
                    val started = service.withClient { it.captureStart() }
                    notifyArb(e, "ライブキャプチャを開始しました (端末: ${started.serial})。")
                }
            } catch (t: Throwable) {
                notifyArb(e, "キャプチャ切替に失敗しました: ${t.message}", NotificationType.ERROR)
            }
        }
    }
}

class ArbOpenBundleAction : AnAction(), DumbAware {
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

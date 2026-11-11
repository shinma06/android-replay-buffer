package io.github.shinma06.replaybuffer.ui

import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.DumbAwareToggleAction
import io.github.shinma06.replaybuffer.core.SaveContent
import io.github.shinma06.replaybuffer.core.SaveOrigin
import io.github.shinma06.replaybuffer.core.SaveRequest
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore

// Running Devices and Logcat toolbar entries; they reuse the ToolWindow's service calls and enablement rules.

private val AnActionEvent.replay: ReplayProjectService?
    get() = project?.takeUnless { it.isDisposed }?.getService(ReplayProjectService::class.java)

private val ReplayProjectService.busy: Boolean get() = resolving || captureView.pending > 0

class ReplayCaptureToggleAction : DumbAwareToggleAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.EDT

    override fun isSelected(event: AnActionEvent): Boolean =
        event.project?.takeUnless { it.isDisposed }?.getService(ReplaySettingsStore::class.java)?.enabled == true

    override fun setSelected(event: AnActionEvent, state: Boolean) {
        event.replay?.setEnabled(state)
    }

    override fun update(event: AnActionEvent) {
        super.update(event)
        val service = event.replay
        val enabled = isSelected(event)
        event.presentation.icon = AllIcons.Actions.Restart
        event.presentation.text = if (enabled) "Replay Bufferの取得を無効にする" else "Replay Bufferの取得を有効にする"
        event.presentation.isEnabled = service != null && (enabled || !service.busy)
    }
}

class ReplaySaveAction : DumbAwareAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.EDT

    override fun actionPerformed(event: AnActionEvent) {
        event.replay?.save(SaveRequest(origin = SaveOrigin.RUNNING_DEVICES))
    }

    override fun update(event: AnActionEvent) {
        val service = event.replay
        val snapshot = service?.captureView?.snapshot
        val seconds = snapshot?.settings?.replaySeconds
            ?: event.project?.getService(ReplaySettingsStore::class.java)?.settings()?.retentionSeconds
        event.presentation.icon = AllIcons.Actions.MenuSaveall
        event.presentation.text = if (seconds != null) "Replay Buffer: 直前${seconds}秒を保存" else "Replay Buffer: 直前の記録を保存"
        event.presentation.description = when {
            service == null -> "projectを開いてください。"
            service.busy -> "設定・操作を反映中です。"
            else -> snapshot?.saveDisabledReason ?: "直前の画面とログを保存します。"
        }
        event.presentation.isEnabled = service != null && !service.busy && snapshot?.canSave == true
    }
}

class ReplayScreenshotAction : DumbAwareAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.EDT

    override fun actionPerformed(event: AnActionEvent) {
        event.replay?.screenshot()
    }

    override fun update(event: AnActionEvent) {
        val service = event.replay
        val reason = service?.screenshotDisabledReason
        event.presentation.icon = AllIcons.FileTypes.Image
        event.presentation.text = if (service?.screenshotBusy == true) "Replay Buffer: スクショを保存中…" else "Replay Buffer: スクリーンショット"
        event.presentation.description = if (service == null) "projectを開いてください。" else reason ?: "録画対象の端末の現在画面を保存します。"
        event.presentation.isEnabled = service != null && reason == null
    }
}

/** Logcat toolbar: saves only logs, replay.json and README.md, even while video is captured; video acquisition keeps running. */
class ReplayLogSaveAction : DumbAwareAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.EDT

    override fun actionPerformed(event: AnActionEvent) {
        event.replay?.save(SaveRequest(SaveContent.LOGS_ONLY, SaveOrigin.LOGCAT_TOOLBAR))
    }

    override fun update(event: AnActionEvent) {
        val service = event.replay
        val snapshot = service?.captureView?.snapshot
        val seconds = snapshot?.settings?.replaySeconds
            ?: event.project?.getService(ReplaySettingsStore::class.java)?.settings()?.retentionSeconds
        event.presentation.icon = AllIcons.Actions.MenuSaveall
        event.presentation.text = if (seconds != null) "Replay Buffer: 直前${seconds}秒のログを保存" else "Replay Buffer: 直前のログを保存"
        event.presentation.description = when {
            service == null -> "projectを開いてください。"
            service.busy -> "設定・操作を反映中です。"
            else -> snapshot?.saveDisabledReason(SaveContent.LOGS_ONLY) ?: "端末ログ・アプリログだけを保存します。動画の取得中でも動画は保存せず、取得も止めません。"
        }
        event.presentation.isEnabled = service != null && !service.busy && snapshot?.canSave(SaveContent.LOGS_ONLY) == true
    }
}

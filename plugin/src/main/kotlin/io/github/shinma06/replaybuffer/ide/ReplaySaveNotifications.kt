package io.github.shinma06.replaybuffer.ide

import com.intellij.notification.Notification
import com.intellij.notification.NotificationDisplayType
import com.intellij.notification.impl.NotificationsConfigurationImpl
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.project.Project
import com.intellij.ui.SystemNotifications
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.menubar.ReplayMenuBarStore

/** One popup channel; the caller still publishes the original IDE notification with its history and actions. */
internal fun notifyBackgroundSave(save: SaveSnapshot, notification: Notification, project: Project) {
    val title = when {
        save.phase == SavePhase.FAILED -> "保存に失敗しました"
        save.phase == SavePhase.COMPLETED && save.directory != null && save.autoEventId != null -> "クラッシュ時の記録を自動保存しました"
        else -> return
    }
    val application = ApplicationManager.getApplication()
    if (project.isDisposed || application.isActive || application.isHeadlessEnvironment || application.isUnitTestMode ||
        application.getService(ReplayMenuBarStore::class.java)?.showInMenuBar != true) return
    val configuration = NotificationsConfigurationImpl.getInstanceImpl()
    val group = NotificationsConfigurationImpl.getSettings(notification.groupId)
    if (!configuration.SYSTEM_NOTIFICATIONS || !configuration.areNotificationsEnabled() ||
        group.displayType == NotificationDisplayType.NONE || !group.isShouldLog) return
    try {
        SystemNotifications.getInstance().notify(notification.id, "${project.name} — $title",
            "Android Replay Bufferの通知から詳細を確認できます。")
        notification.setSuppressShowingPopup(true)
    } catch (cancelled: ProcessCanceledException) {
        throw cancelled
    } catch (failure: Exception) {
        Logger.getInstance("io.github.shinma06.replaybuffer.ide.ReplaySaveNotifications").warn("OS通知へ送れませんでした。IDE通知を残します。", failure)
    }
}

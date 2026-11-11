package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowManagerListener

internal const val LOGCAT_TOOL_WINDOW_ID = "Logcat"
internal const val LOGCAT_SAVE_LOGS_ACTION_ID = "io.github.shinma06.replaybuffer.Logcat.SaveLogs"

/**
 * Logcat's own toolbar is a fixed group without a public extension group, so the logs-only save goes to the
 * Logcat tool window header through the public ToolWindow.setTitleActions (Logcat sets only tab actions).
 */
class LogcatTitleActions(private val project: Project) : ToolWindowManagerListener {
    override fun toolWindowsRegistered(ids: List<String>, toolWindowManager: ToolWindowManager) {
        if (LOGCAT_TOOL_WINDOW_ID !in ids) return
        toolWindowManager.invokeLater {
            if (project.isDisposed) return@invokeLater
            val action = ActionManager.getInstance().getAction(LOGCAT_SAVE_LOGS_ACTION_ID) ?: return@invokeLater
            toolWindowManager.getToolWindow(LOGCAT_TOOL_WINDOW_ID)?.setTitleActions(listOf(action))
        }
    }
}

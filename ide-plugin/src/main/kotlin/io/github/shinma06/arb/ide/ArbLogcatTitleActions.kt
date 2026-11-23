package io.github.shinma06.arb.ide

import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowManagerListener

internal const val LOGCAT_TOOL_WINDOW_ID: String = "Logcat"
internal const val ARB_LOGCAT_SAVE_LOGS_ACTION_ID: String = "io.github.shinma06.arb.Logcat.SaveLogs"

/**
 * Attaches the `ARB v2: 直前のログのみ保存` action to Android Studio's Logcat tool window
 * title bar (mirrors v1's public `ToolWindow.setTitleActions` hook).
 */
class ArbLogcatTitleActions(private val project: Project) : ToolWindowManagerListener {
    override fun toolWindowsRegistered(ids: List<String>, toolWindowManager: ToolWindowManager) {
        if (LOGCAT_TOOL_WINDOW_ID !in ids) return
        toolWindowManager.invokeLater {
            if (project.isDisposed) return@invokeLater
            val action = ActionManager.getInstance().getAction(ARB_LOGCAT_SAVE_LOGS_ACTION_ID)
                ?: return@invokeLater
            toolWindowManager.getToolWindow(LOGCAT_TOOL_WINDOW_ID)?.setTitleActions(listOf(action))
        }
    }
}

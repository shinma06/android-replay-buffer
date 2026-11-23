package io.github.shinma06.arb.ide

import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.Separator
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowManagerListener

internal const val LOGCAT_TOOL_WINDOW_ID: String = "Logcat"
internal const val ARB_LOGCAT_SAVE_LOGS_ACTION_ID: String = "io.github.shinma06.arb.Logcat.SaveLogs"

/**
 * Non-destructively appends the `ARB v2: 直前のログのみ保存` action to Android Studio's Logcat
 * tool window title bar while preserving any existing built-in Logcat title actions.
 */
class ArbLogcatTitleActions(private val project: Project) : ToolWindowManagerListener {
    override fun toolWindowsRegistered(ids: List<String>, toolWindowManager: ToolWindowManager) {
        if (LOGCAT_TOOL_WINDOW_ID !in ids) return
        toolWindowManager.invokeLater {
            if (project.isDisposed) return@invokeLater
            val action = ActionManager.getInstance().getAction(ARB_LOGCAT_SAVE_LOGS_ACTION_ID)
                ?: return@invokeLater
            val toolWindow = toolWindowManager.getToolWindow(LOGCAT_TOOL_WINDOW_ID) ?: return@invokeLater
            appendTitleActionNonDestructively(toolWindow, action)
        }
    }

    companion object {
        internal fun appendTitleActionNonDestructively(toolWindow: ToolWindow, action: AnAction) {
            val existing = extractExistingTitleActions(toolWindow)
            val merged = mergeTitleActions(existing, action)
            toolWindow.setTitleActions(merged)
        }

        /**
         * Pure merge helper: preserves all existing title actions in order (dropping any synthetic
         * trailing separators added by old-UI ToolWindowHeader) and appends [actionToAppend] only
         * if it is not already present (idempotent).
         */
        internal fun mergeTitleActions(
            existing: List<AnAction>,
            actionToAppend: AnAction,
            actionIdResolver: (AnAction) -> String? = { candidate ->
                runCatching { ActionManager.getInstance()?.getId(candidate) }.getOrNull()
            },
        ): List<AnAction> {
            val cleaned = existing.dropLastWhile { it is Separator }
            val targetId = actionIdResolver(actionToAppend)
            val alreadyPresent = cleaned.any { candidate ->
                candidate === actionToAppend ||
                    candidate.javaClass == actionToAppend.javaClass ||
                    (targetId != null && actionIdResolver(candidate) == targetId)
            }
            return if (alreadyPresent) cleaned else cleaned + actionToAppend
        }

        private fun extractExistingTitleActions(toolWindow: ToolWindow): List<AnAction> = runCatching {
            val getDecorator = toolWindow.javaClass.methods.firstOrNull {
                it.name == "getDecorator" && it.parameterCount == 0
            } ?: return emptyList()
            val decorator = getDecorator.invoke(toolWindow) ?: return emptyList()
            val getHeaderToolbarActions = decorator.javaClass.methods.firstOrNull {
                it.name == "getHeaderToolbarActions" && it.parameterCount == 0
            } ?: return emptyList()
            val group = getHeaderToolbarActions.invoke(decorator) as? DefaultActionGroup ?: return emptyList()
            group.getChildActionsOrStubs().toList()
        }.getOrDefault(emptyList())
    }
}

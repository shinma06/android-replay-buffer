package io.github.shinma06.arb.ide

import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.content.ContentFactory
import io.github.shinma06.arb.ui.ArbToolWindowPanel

class ArbToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val daemonService = ArbdDaemonService.getInstance()
        val panel = ArbToolWindowPanel(
            daemonService = daemonService,
            onOpenBundle = { bundleIdOrPath, title, isLegacy ->
                val vf = ArbBundleVirtualFile(bundleIdOrPath, title, isLegacy)
                FileEditorManager.getInstance(project).openFile(vf, true)
            },
            onOpenSettings = {
                if (!project.isDisposed) {
                    ShowSettingsUtil.getInstance().showSettingsDialog(project, ArbSettingsConfigurable::class.java)
                }
            },
        )
        val content = ContentFactory.getInstance().createContent(panel, "", false)
        Disposer.register(content, panel)
        toolWindow.contentManager.addContent(content)
    }
}

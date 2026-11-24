package io.github.shinma06.arb.ide

import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.content.ContentFactory
import io.github.shinma06.arb.ui.ArbToolWindowContent
import io.github.shinma06.arb.ui.ArbToolWindowController
import org.jetbrains.jewel.bridge.JewelComposePanel
import java.awt.BorderLayout
import javax.swing.JPanel

class ArbToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val daemonService = ArbdDaemonService.getInstance()
        val controller = ArbToolWindowController(
            project = project,
            daemonService = daemonService,
            onOpenBundle = { bundleIdOrPath, title, isLegacy ->
                if (!project.isDisposed) {
                    val vf = ArbBundleVirtualFile(bundleIdOrPath, title, isLegacy)
                    FileEditorManager.getInstance(project).openFile(vf, true)
                }
            },
            onOpenSettings = {
                if (!project.isDisposed) {
                    ShowSettingsUtil.getInstance().showSettingsDialog(project, ArbSettingsConfigurable::class.java)
                }
            },
        )
        val wrapper = JPanel(BorderLayout()).apply {
            add(JewelComposePanel { ArbToolWindowContent(controller) }, BorderLayout.CENTER)
        }
        val content = ContentFactory.getInstance().createContent(wrapper, "", false)
        Disposer.register(content, controller)
        toolWindow.contentManager.addContent(content)
        toolWindow.setTitleActions(listOf(ArbLaunchTrayAction()))
    }
}

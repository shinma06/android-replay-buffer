package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.Disposable
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.ui.components.JBLabel
import com.intellij.ui.content.ContentFactory
import com.intellij.util.ui.FormBuilder
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurable
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import java.awt.BorderLayout
import javax.swing.JButton
import javax.swing.JPanel

class ReplayToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = ReplayToolWindowPanel(project)
        val content = ContentFactory.getInstance().createContent(panel, "", false)
        Disposer.register(content, panel)
        content.setPreferredFocusableComponent(panel.settingsButton)
        toolWindow.contentManager.addContent(content)
    }
}

private class ReplayToolWindowPanel(private val project: Project) : JPanel(BorderLayout()), Disposable {
    private val application = JBLabel("アプリ: 確認中…")
    private val sdk = JBLabel("Android SDK: 確認中…")
    private val retention = JBLabel()
    private val destination = JBLabel()
    private var disposed = false
    val settingsButton = JButton("設定…").apply {
        toolTipText = "取得が無効でも設定を編集できます"
        addActionListener {
            if (!disposed && !project.isDisposed) {
                ShowSettingsUtil.getInstance().showSettingsDialog(project, ReplaySettingsConfigurable::class.java)
            }
        }
    }

    init {
        val form = FormBuilder.createFormBuilder()
            .addComponent(settingsButton)
            .addComponent(application)
            .addComponent(sdk)
            .addComponent(retention)
            .addComponent(destination)
            .addComponent(JBLabel("取得・保存機能はまだ接続されていません。"))
            .addComponentFillVertically(JPanel(), 0)
            .panel
        add(form, BorderLayout.CENTER)
        val connection = project.messageBus.connect(this)
        val refresh = Runnable {
            if (!project.isDisposed) {
                ToolWindowManager.getInstance(project).invokeLater {
                    if (!disposed && !project.isDisposed) render()
                }
            }
        }
        connection.subscribe(ReplayProjectService.ENVIRONMENT_CHANGED, refresh)
        connection.subscribe(ReplaySettingsStore.CHANGED, refresh)
        render()
        project.getService(ReplayProjectService::class.java).refresh()
    }

    private fun render() {
        val settings = project.getService(ReplaySettingsStore::class.java).settings()
        val environment = project.getService(ReplayProjectService::class.java).environment
        val selection = environment?.application
        application.text = "アプリ: " + (selection?.packageName ?: selection?.reason ?: "確認中…") + "（${settings.appSelection}）"
        sdk.text = "Android SDK: " + (environment?.adb?.toString() ?: environment?.adbReason ?: "確認中…")
        retention.text = "保持時間: ${settings.retentionSeconds}秒"
        destination.text = "保存先: ${settings.destination.ifBlank { "設定で指定してください" }}"
    }

    override fun dispose() {
        disposed = true
    }
}

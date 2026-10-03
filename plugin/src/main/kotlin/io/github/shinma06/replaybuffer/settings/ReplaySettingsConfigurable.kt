package io.github.shinma06.replaybuffer.settings

import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.Configurable
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.TextFieldWithBrowseButton
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBTextField
import com.intellij.util.ui.FormBuilder
import javax.swing.JComboBox
import javax.swing.JComponent
import javax.swing.JPanel

class ReplaySettingsConfigurable(private val project: Project) : Configurable {
    private var panel: JPanel? = null
    private var retention: JBTextField? = null
    private var destination: TextFieldWithBrowseButton? = null
    private var mode: JComboBox<AppSelectionMode>? = null
    private var manualPackage: JBTextField? = null

    override fun getDisplayName(): String = "Android Replay Buffer"

    override fun createComponent(): JComponent {
        retention = JBTextField().apply { accessibleContext.accessibleName = "保持時間（1〜900秒）" }
        destination = TextFieldWithBrowseButton().apply {
            addBrowseFolderListener(project, FileChooserDescriptorFactory.createSingleFolderDescriptor())
            accessibleContext.accessibleName = "保存先フォルダ"
        }
        manualPackage = JBTextField().apply { accessibleContext.accessibleName = "対象アプリのpackage名" }
        mode = JComboBox(AppSelectionMode.entries.toTypedArray()).apply {
            accessibleContext.accessibleName = "対象アプリの選択方法"
            addActionListener { manualPackage?.isEnabled = selectedItem == AppSelectionMode.MANUAL }
        }
        panel = FormBuilder.createFormBuilder()
            .addLabeledComponent("保持時間（1〜900秒）:", retention!!)
            .addLabeledComponent("保存先フォルダ:", destination!!)
            .addLabeledComponent("対象アプリ:", mode!!)
            .addLabeledComponent("package名:", manualPackage!!)
            .addComponent(JBLabel("編集だけでは取得条件を変更しません。「適用」または「OK」で反映します。"))
            .addComponent(JBLabel("取得の有効・無効はToolWindowから即時に切り替えます。"))
            .addComponent(JBLabel("保存先が未指定でも取得できます。保存する前にフォルダを指定してください。"))
            .addComponentFillVertically(JPanel(), 0)
            .panel
        return panel!!
    }

    private fun draft(): ReplaySettings = ReplaySettings(
        retentionSeconds = retention?.text?.toIntOrNull() ?: 0,
        destination = destination?.text.orEmpty(),
        appSelection = mode?.selectedItem as? AppSelectionMode ?: AppSelectionMode.AUTOMATIC,
        manualPackage = manualPackage?.text.orEmpty(),
    )

    override fun isModified(): Boolean = panel != null && draft() != store().settings()

    override fun apply() {
        if (panel == null || project.isDisposed || !isModified()) return
        val settings = draft()
        settings.validationError()?.let { throw ConfigurationException(it) }
        store().apply(settings)
        project.messageBus.syncPublisher(ReplaySettingsStore.CHANGED).run()
    }

    override fun reset() {
        if (panel == null || project.isDisposed) return
        val settings = store().settings()
        retention?.text = settings.retentionSeconds.toString()
        destination?.text = settings.destination
        mode?.selectedItem = settings.appSelection
        manualPackage?.text = settings.manualPackage
        manualPackage?.isEnabled = settings.appSelection == AppSelectionMode.MANUAL
    }

    override fun disposeUIResources() {
        destination?.dispose()
        panel = null
        retention = null
        destination = null
        mode = null
        manualPackage = null
    }

    private fun store(): ReplaySettingsStore = project.getService(ReplaySettingsStore::class.java)
}

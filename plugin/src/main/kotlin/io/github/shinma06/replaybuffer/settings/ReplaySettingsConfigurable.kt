package io.github.shinma06.replaybuffer.settings

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.Configurable
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.TextFieldWithBrowseButton
import com.intellij.openapi.util.Disposer
import com.intellij.ui.DocumentAdapter
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBTextField
import com.intellij.util.Alarm
import com.intellij.util.ui.FormBuilder
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import javax.swing.JComboBox
import javax.swing.JComponent
import javax.swing.JPanel
import javax.swing.event.DocumentEvent

class ReplaySettingsConfigurable(private val project: Project) : Configurable {
    private var panel: JPanel? = null
    private var retention: JBTextField? = null
    private var destination: TextFieldWithBrowseButton? = null
    private var mode: JComboBox<AppSelectionMode>? = null
    private var manualPackage: JBTextField? = null
    private var destinationStatus: JBLabel? = null
    private var validationAlarm: Alarm? = null
    private var destinationValidation: DestinationValidation? = null
    private var destinationRevision = 0L
    private var replayService: ReplayProjectService? = null
    private var applicationStatus: JBLabel? = null

    override fun getDisplayName(): String = "Android Replay Buffer"

    override fun createComponent(): JComponent {
        validationAlarm = Alarm(Alarm.ThreadToUse.POOLED_THREAD, project)
        replayService = project.getService(ReplayProjectService::class.java)
        applicationStatus = JBLabel().apply { accessibleContext.accessibleName = "対象アプリの解決結果" }
        val lifetime = validationAlarm!!
        project.messageBus.connect(lifetime).subscribe(ReplayProjectService.ENVIRONMENT_CHANGED, Runnable {
            ApplicationManager.getApplication().invokeLater({
                if (validationAlarm === lifetime && !project.isDisposed) renderApplication()
            }, ModalityState.any())
        })
        destinationStatus = JBLabel().apply { accessibleContext.accessibleName = "保存先の確認結果" }
        retention = JBTextField().apply { accessibleContext.accessibleName = "保持時間（1〜900秒）" }
        destination = TextFieldWithBrowseButton().apply {
            addBrowseFolderListener(project, FileChooserDescriptorFactory.createSingleFolderDescriptor())
            accessibleContext.accessibleName = "保存先フォルダ"
            textField.document.addDocumentListener(object : DocumentAdapter() {
                override fun textChanged(event: DocumentEvent) = checkDestination()
            })
        }
        manualPackage = JBTextField().apply { accessibleContext.accessibleName = "対象アプリのpackage名" }
        mode = JComboBox(AppSelectionMode.entries.toTypedArray()).apply {
            accessibleContext.accessibleName = "対象アプリの選択方法"
            addActionListener { manualPackage?.isEnabled = selectedItem == AppSelectionMode.MANUAL }
        }
        panel = FormBuilder.createFormBuilder()
            .addLabeledComponent("保持時間（1〜900秒）:", retention!!)
            .addLabeledComponent("保存先フォルダ:", destination!!)
            .addComponent(destinationStatus!!)
            .addLabeledComponent("対象アプリ:", mode!!)
            .addLabeledComponent("package名:", manualPackage!!)
            .addComponent(applicationStatus!!)
            .addComponent(JBLabel("編集だけでは取得条件を変更しません。「適用」または「OK」で反映します。"))
            .addComponent(JBLabel("取得の有効・無効はToolWindowから即時に切り替えます。"))
            .addComponentFillVertically(JPanel(), 0)
            .panel
        checkDestination()
        renderApplication()
        return panel!!
    }

    private fun renderApplication() {
        val application = replayService?.environment?.application
        applicationStatus?.text = "対象の解決結果: ${application?.packageName ?: application?.reason ?: "確認中…"}" +
            (application?.configurationName?.let { "（Run: $it）" } ?: "")
    }

    private fun checkDestination() {
        val field = destination ?: return
        val alarm = validationAlarm ?: return
        val value = field.text
        val revision = ++destinationRevision
        alarm.cancelAllRequests()
        destinationValidation = null
        destinationStatus?.text = if (value.isEmpty()) {
            "保存先が未指定でも取得できます。保存する前にフォルダを指定してください。"
        } else {
            "保存先を確認中…"
        }
        if (value.isEmpty()) {
            destinationValidation = validateDestination(value)
            return
        }
        alarm.addRequest({
            val result = validateDestination(value)
            ApplicationManager.getApplication().invokeLater({
                if (validationAlarm !== alarm || project.isDisposed || revision != destinationRevision) return@invokeLater
                destinationValidation = result
                destinationStatus?.text = result.error ?: " "
            }, ModalityState.any())
        }, 150)
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
        val checked = destinationValidation?.takeIf { it.destination == settings.destination }
            ?: throw ConfigurationException("保存先を確認中です。確認が終わってから適用してください。")
        settings.validationError(checked)?.let { throw ConfigurationException(it) }
        store().apply(settings, checked)
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
        checkDestination()
        renderApplication()
    }

    override fun disposeUIResources() {
        validationAlarm?.let { Disposer.dispose(it) }
        validationAlarm = null
        destinationValidation = null
        destinationStatus = null
        applicationStatus = null
        replayService = null
        destination?.dispose()
        panel = null
        retention = null
        destination = null
        mode = null
        manualPackage = null
    }

    private fun store(): ReplaySettingsStore = project.getService(ReplaySettingsStore::class.java)
}

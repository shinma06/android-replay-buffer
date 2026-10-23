package io.github.shinma06.replaybuffer.settings

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.application.UI
import com.intellij.openapi.application.asContextElement
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.Configurable
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.TextFieldWithBrowseButton
import com.intellij.openapi.util.Disposer
import com.intellij.ui.DocumentAdapter
import com.intellij.ui.TitledSeparator
import com.intellij.ui.components.JBCheckBox
import com.intellij.ui.components.JBTextArea
import com.intellij.ui.components.JBTextField
import com.intellij.util.ui.FormBuilder
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.ide.LogcatScreenshotSettings
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.ui.captureModeDescription
import io.github.shinma06.replaybuffer.ui.wrappingLabel
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import javax.swing.JComboBox
import javax.swing.JComponent
import javax.swing.JPanel
import javax.swing.event.DocumentEvent

/** Owns an EDT form draft; only Apply persists it, and async checks are bound to form identity and revision. */
class ReplaySettingsConfigurable(private val project: Project) : Configurable {
    private var panel: JPanel? = null
    private var retention: JBTextField? = null
    private var destination: TextFieldWithBrowseButton? = null
    private var screenshotLogcat: JBCheckBox? = null
    private var screenshotDirectory: TextFieldWithBrowseButton? = null
    private var screenshotStatus: JBTextArea? = null
    private var screenshotValidation: DestinationValidation? = null
    private var screenshotValidationJob: Job? = null
    private var screenshotRevision = 0L
    private var mode: JComboBox<AppSelectionMode>? = null
    private var captureMode: JComboBox<CaptureMode>? = null
    private var autoSave: JBCheckBox? = null
    private var manualPackage: JBTextField? = null
    private var destinationStatus: JBTextArea? = null
    private var uiLifetime: Disposable? = null
    private var validationJob: Job? = null
    private var destinationValidation: DestinationValidation? = null
    private var destinationRevision = 0L
    private var replayService: ReplayProjectService? = null
    private var applicationStatus: JBTextArea? = null

    override fun getDisplayName(): String = "Android Replay Buffer"

    override fun createComponent(): JComponent {
        replayService = project.getService(ReplayProjectService::class.java)
        applicationStatus = wrappingLabel(name = "対象アプリの解決結果")
        val lifetime = Disposer.newDisposable("Replay Buffer settings UI")
        uiLifetime = lifetime
        Disposer.register(replayService ?: project, lifetime)
        Disposer.register(lifetime) { validationJob?.cancel(); screenshotValidationJob?.cancel() }
        project.messageBus.connect(lifetime).subscribe(ReplayProjectService.ENVIRONMENT_CHANGED, Runnable {
            ApplicationManager.getApplication().invokeLater({
                if (uiLifetime === lifetime && !project.isDisposed) renderApplication()
            }, ModalityState.any())
        })
        destinationStatus = wrappingLabel(name = "保存先の確認結果")
        retention = JBTextField().apply { accessibleContext.accessibleName = "保持時間（1〜900秒）" }
        destination = TextFieldWithBrowseButton().apply {
            addBrowseFolderListener(project, FileChooserDescriptorFactory.createSingleFolderDescriptor())
            accessibleContext.accessibleName = "保存先フォルダ"
            textField.document.addDocumentListener(object : DocumentAdapter() {
                override fun textChanged(event: DocumentEvent) = checkDestination()
            })
        }
        screenshotStatus = wrappingLabel(name = "スクリーンショット保存先の確認結果")
        screenshotDirectory = TextFieldWithBrowseButton().apply {
            addBrowseFolderListener(project, FileChooserDescriptorFactory.createSingleFolderDescriptor())
            accessibleContext.accessibleName = "スクリーンショットの手動保存先"
            textField.document.addDocumentListener(object : DocumentAdapter() {
                override fun textChanged(event: DocumentEvent) = checkScreenshotDestination()
            })
        }
        screenshotLogcat = JBCheckBox("Logcatのスクショ保存先と同じ場所に保存する", true).apply {
            accessibleContext.accessibleName = "スクリーンショットをLogcatのスクショ保存先へ保存"
            toolTipText = "Logcatの画面撮影設定に追従します。OFFでは手動指定できます。動画・ログと同じフォルダも使えます。"
            addActionListener { checkScreenshotDestination() }
        }
        val packageGuidance = "255文字以内。英字で始まる2つ以上の部分をピリオドで区切ります。" +
            "各部分には半角英数字と「_」を使えます（例: com.example.app）。"
        manualPackage = JBTextField().apply {
            accessibleContext.accessibleName = "対象アプリのpackage名"
            accessibleContext.accessibleDescription = packageGuidance
        }
        mode = JComboBox(AppSelectionMode.entries.toTypedArray()).apply {
            accessibleContext.accessibleName = "対象アプリの選択方法"
            addActionListener { manualPackage?.isEnabled = selectedItem == AppSelectionMode.MANUAL }
        }
        captureMode = JComboBox(CaptureMode.entries.toTypedArray()).apply {
            accessibleContext.accessibleName = "取得モード"
            accessibleContext.accessibleDescription = "取得をOFFにしてから変更してください。ログ専用は動画を取得・保存しません。"
            renderer = object : javax.swing.DefaultListCellRenderer() {
                override fun getListCellRendererComponent(list: javax.swing.JList<*>?, value: Any?, index: Int, selected: Boolean, focus: Boolean): java.awt.Component =
                    super.getListCellRendererComponent(list, (value as? CaptureMode)?.let(::captureModeDescription), index, selected, focus)
            }
        }
        autoSave = JBCheckBox("クラッシュ報告で自動保存する").apply {
            accessibleContext.accessibleName = "クラッシュ報告の自動保存"
            toolTipText = "既定OFF。適用後の報告が対象です。native/ANRは対応外。8回受付または検出上限で停止します。再開は自動保存をOFFで適用してからONで適用してください。"
        }
        panel = FormBuilder.createFormBuilder()
            .addComponent(wrappingLabel("編集内容は「適用」または「OK」で反映します。キャンセルでは変更しません。" +
                "取得のON/OFFはToolWindowですぐ反映します。", "設定の適用方法"))
            .addComponent(TitledSeparator("対象アプリ"))
            .addLabeledComponent("選択方法:", mode!!)
            .addLabeledComponent("package名:", manualPackage!!)
            .addComponent(wrappingLabel(packageGuidance, "package名の入力条件"))
            .addComponent(applicationStatus!!)
            .addComponent(TitledSeparator("取得する記録"))
            .addLabeledComponent("取得モード:", captureMode!!)
            .addComponent(wrappingLabel("取得モードは取得OFFで変更してください。ログ専用は動画を取得・保存しません。"))
            .addLabeledComponent("保持時間（1〜900秒）:", retention!!)
            .addComponent(autoSave!!)
            .addComponent(wrappingLabel("対象アプリの標準Java/Kotlinクラッシュ報告が対象です。native/ANRは対応外です。" +
                "8回受付または検出上限で停止します。再開はこの設定をOFFで適用してから、ONで再び適用してください。", "クラッシュ自動保存の対応範囲"))
            .addComponent(TitledSeparator("動画・ログの保存先"))
            .addLabeledComponent("保存先フォルダ:", destination!!)
            .addComponent(destinationStatus!!)
            .addComponent(TitledSeparator("スクリーンショットの保存先"))
            .addComponent(screenshotLogcat!!)
            .addLabeledComponent("手動の保存先フォルダ:", screenshotDirectory!!)
            .addComponent(screenshotStatus!!)
            .addComponentFillVertically(JPanel(), 0)
            .panel
        checkDestination()
        checkScreenshotDestination()
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
        val lifetime = uiLifetime ?: return
        val value = field.text
        val revision = ++destinationRevision
        validationJob?.cancel()
        validationJob = null
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
        validationJob = replayService?.scope?.checkDestination(value) { result ->
            if (uiLifetime === lifetime && !project.isDisposed && revision == destinationRevision) {
                destinationValidation = result
                destinationStatus?.text = result.error ?: " "
            }
        }
    }

    private fun checkScreenshotDestination() {
        val field = screenshotDirectory ?: return
        val lifetime = uiLifetime ?: return
        val followsLogcat = screenshotLogcat?.isSelected != false
        field.isEnabled = !followsLogcat
        val revision = ++screenshotRevision
        screenshotValidationJob?.cancel()
        screenshotValidationJob = null
        screenshotValidation = null
        val location = if (followsLogcat) {
            try { LogcatScreenshotSettings.saveLocation() } catch (cancelled: com.intellij.openapi.progress.ProcessCanceledException) {
                throw cancelled
            } catch (_: RuntimeException) {
                screenshotStatus?.text = LOGCAT_DESTINATION_UNAVAILABLE
                return
            } catch (_: LinkageError) {
                screenshotStatus?.text = LOGCAT_DESTINATION_UNAVAILABLE
                return
            }
        } else null
        val value = location ?: field.text
        screenshotStatus?.text = if (!followsLogcat && value.isEmpty()) "スクリーンショット専用の保存先を指定してください。" else "保存先を確認中…"
        if (!followsLogcat && value.isEmpty()) {
            screenshotValidation = validateDestination(value)
            return
        }
        screenshotValidationJob = replayService?.scope?.checkDestination(value, {
            if (location == null) value else LogcatScreenshotSettings.resolveDirectory(project, location).toString()
        }) { result ->
            if (uiLifetime === lifetime && !project.isDisposed && revision == screenshotRevision) {
                screenshotValidation = result
                screenshotStatus?.text = if (followsLogcat) {
                    "Logcatのスクショ保存先: ${result.destination}。" +
                        (result.error ?: "保存時の最新設定を使い、直下へPNG1枚を保存します。IDE設定の初期値はデスクトップです。")
                } else result.error ?: "専用フォルダ直下にPNG画像1枚を保存します。"
            }
        }
    }

    private fun draft(): ReplaySettings = ReplaySettings(
        retentionSeconds = retention?.text?.toIntOrNull() ?: 0,
        destination = destination?.text.orEmpty(),
        appSelection = mode?.selectedItem as? AppSelectionMode ?: AppSelectionMode.AUTOMATIC,
        manualPackage = manualPackage?.text.orEmpty(),
        captureMode = captureMode?.selectedItem as? CaptureMode ?: CaptureMode.VIDEO_AND_LOGS,
        autoSaveOnCrash = autoSave?.isSelected == true,
        screenshotUsesLogcatDestination = screenshotLogcat?.isSelected != false,
        screenshotDirectory = screenshotDirectory?.text.orEmpty(),
    )

    override fun isModified(): Boolean = panel != null && draft() != store().settings()

    override fun apply() {
        if (panel == null || project.isDisposed || !isModified()) return
        val settings = draft()
        val checked = destinationValidation?.takeIf { it.destination == settings.destination }
            ?: throw ConfigurationException("保存先を確認中です。確認が終わってから適用してください。")
        val screenshotChecked = if (settings.screenshotUsesLogcatDestination) DestinationValidation("", null) else {
            screenshotValidation?.takeIf { it.destination == settings.screenshotDirectory }
                ?: throw ConfigurationException("スクリーンショットの保存先を確認中です。確認が終わってから適用してください。")
        }
        settings.validationError(checked, screenshotChecked)?.let { throw ConfigurationException(it) }
        val service = replayService
        if (settings.captureMode != store().settings().captureMode &&
            (store().enabled || service?.captureView?.snapshot?.enabled == true || service?.resolving == true || (service?.captureView?.pending ?: 0) > 0))
            throw ConfigurationException("取得をOFFにし、設定・操作の反映が終わってから取得モードを変更してください。")
        try { store().apply(settings, checked, screenshotChecked) } catch (failure: IllegalArgumentException) {
            throw ConfigurationException(failure.message.orEmpty())
        }
        project.messageBus.syncPublisher(ReplaySettingsStore.CHANGED).run()
    }

    override fun reset() {
        if (panel == null || project.isDisposed) return
        val settings = store().settings()
        retention?.text = settings.retentionSeconds.toString()
        destination?.text = settings.destination
        screenshotLogcat?.isSelected = settings.screenshotUsesLogcatDestination
        screenshotDirectory?.text = settings.screenshotDirectory
        mode?.selectedItem = settings.appSelection
        captureMode?.selectedItem = settings.captureMode
        autoSave?.isSelected = settings.autoSaveOnCrash
        manualPackage?.text = settings.manualPackage
        manualPackage?.isEnabled = settings.appSelection == AppSelectionMode.MANUAL
        checkDestination()
        checkScreenshotDestination()
        renderApplication()
    }

    override fun disposeUIResources() {
        uiLifetime?.let { Disposer.dispose(it) }
        uiLifetime = null
        validationJob = null
        destinationValidation = null
        screenshotValidation = null
        screenshotValidationJob = null
        screenshotStatus = null
        destinationStatus = null
        applicationStatus = null
        replayService = null
        destination?.dispose()
        screenshotDirectory?.dispose()
        panel = null
        retention = null
        destination = null
        screenshotDirectory = null
        screenshotLogcat = null
        mode = null
        captureMode = null
        autoSave = null
        manualPackage = null
    }

    private fun store(): ReplaySettingsStore = project.getService(ReplaySettingsStore::class.java)
}

internal const val LOGCAT_DESTINATION_UNAVAILABLE = "Logcatのスクショ保存先を取得できません。Android Studioの画面撮影設定を確認するか、手動の保存先を指定してください。"

/**
 * The service scope owns the job: resolve/attribute checks run on IO and [publish] returns on UI.
 *
 * Forms cancel their pending job and reject stale lifetime/revision results before touching widgets.
 */
internal fun CoroutineScope.checkDestination(
    value: String,
    resolve: () -> String = { value },
    publish: (DestinationValidation) -> Unit,
): Job =
    launch(Dispatchers.UI + ModalityState.current().asContextElement()) {
        delay(150)
        val result = withContext(Dispatchers.IO) {
            try { validateDestination(resolve()) } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (cancelled: com.intellij.openapi.progress.ProcessCanceledException) {
                throw cancelled
            } catch (_: RuntimeException) {
                DestinationValidation(value, LOGCAT_DESTINATION_UNAVAILABLE)
            } catch (_: LinkageError) {
                DestinationValidation(value, LOGCAT_DESTINATION_UNAVAILABLE)
            }
        }
        publish(result)
    }

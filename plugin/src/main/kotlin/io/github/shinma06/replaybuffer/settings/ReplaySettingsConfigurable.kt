package io.github.shinma06.replaybuffer.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.intellij.ide.plugins.cl.PluginAwareClassLoader
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.application.UI
import com.intellij.openapi.application.asContextElement
import com.intellij.openapi.fileChooser.FileChooser
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.options.Configurable
import com.intellij.openapi.options.ConfigurationException
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.vfs.LocalFileSystem
import io.github.shinma06.replaybuffer.cli.ReplayCliRegistry
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.VideoQuality
import io.github.shinma06.replaybuffer.ide.LogcatScreenshotSettings
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import io.github.shinma06.replaybuffer.ui.captureModeDescription
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.jetbrains.jewel.bridge.JewelComposePanel
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.component.CheckboxRow
import org.jetbrains.jewel.ui.component.GroupHeader
import org.jetbrains.jewel.ui.component.ListComboBox
import org.jetbrains.jewel.ui.component.OutlinedButton
import org.jetbrains.jewel.ui.component.RadioButtonRow
import org.jetbrains.jewel.ui.component.Text
import org.jetbrains.jewel.ui.component.TextField
import org.jetbrains.jewel.ui.component.Tooltip
import org.jetbrains.jewel.ui.component.VerticallyScrollableContainer
import java.awt.datatransfer.StringSelection
import javax.swing.JComponent

internal const val PACKAGE_GUIDANCE = "255文字以内。英字で始まる2つ以上の部分をピリオドで区切ります。" +
    "各部分には半角英数字と「_」を使えます（例: com.example.app）。"
internal const val APPLY_GUIDANCE = "編集内容は「適用」または「OK」で反映します。キャンセルでは変更しません。" +
    "取得のON/OFFはToolWindowですぐ反映します。"
internal const val RECORDING_LOCKED = "取得をOFFにすると変更できます"
internal const val CAPTURE_MODE_TOOLTIP = "取得をOFFにしてから変更してください。ログ専用は動画を取得・保存しません。"
internal const val VIDEO_QUALITY_TOOLTIP = "長辺・fpsは上限、Mbpsは指定bitrateです。取得OFFで変更してください。" +
    "軽量は細かい文字・短い変化・滑らかさを捉えにくくなります。負荷の低下は環境に依存します。" +
    "ログ専用では動画品質を使いません。"
internal const val SHOW_TOUCHES_TOOLTIP = "既定OFF。動画を取得している間は、タップ座標と時刻を自動で記録し、保存時にtouches.jsonlへ動画と同期して書き出します。" +
    "ONにすると、端末の「タップを表示」を取得中だけONにして点を動画に映し、取得終了時に元の値へ戻します。取得OFFで変更してください。"
internal const val AUTO_SAVE_SCOPE = "対象アプリの標準Java/Kotlinクラッシュ報告が対象です。native/ANRは対応外です。"
internal const val AUTO_SAVE_TOOLTIP = "既定OFF。適用後の報告が対象です。" +
    "8回受付または検出上限で停止します。再開はこの設定をOFFで適用してから、ONで再び適用してください。"
internal const val SCREENSHOT_LOGCAT_TOOLTIP = "Logcatの画面撮影設定に追従します。手動では専用の保存先を指定できます。動画・ログと同じフォルダも使えます。"
internal const val CLI_GUIDANCE = "導入済みプラグインのbinにあるreplayで、この画面と同じ設定・取得・保存を操作できます。" +
    "MCPはreplay mcpです。プラグイン更新後も同じ場所を使えます。"

/** Which settings controls accept input; pure so the rules are testable without a UI. */
internal data class SettingsControls(
    val manualPackageEnabled: Boolean,
    val recordingEnabled: Boolean,
    val videoOptionsEnabled: Boolean,
    val screenshotDirectoryEnabled: Boolean,
    /** Shown once under 記録する内容, only while capture blocks changing it. */
    val recordingNotice: String?,
)

internal fun settingsControls(
    appSelection: AppSelectionMode,
    captureMode: CaptureMode,
    screenshotUsesLogcat: Boolean,
    captureActive: Boolean,
): SettingsControls = SettingsControls(
    manualPackageEnabled = appSelection == AppSelectionMode.MANUAL,
    recordingEnabled = !captureActive,
    videoOptionsEnabled = !captureActive && captureMode.videoRequested,
    screenshotDirectoryEnabled = !screenshotUsesLogcat,
    recordingNotice = RECORDING_LOCKED.takeIf { captureActive },
)

/** The EDT draft edited by the form; only [ReplaySettingsConfigurable.apply] persists it. */
internal class SettingsForm(val launcher: String?) {
    val retention = TextFieldState()
    val destination = TextFieldState()
    val manualPackage = TextFieldState()
    val screenshotDirectory = TextFieldState()
    var appSelection by mutableStateOf(AppSelectionMode.AUTOMATIC)
    var captureMode by mutableStateOf(CaptureMode.VIDEO_AND_LOGS)
    var videoQuality by mutableStateOf(VideoQuality.STANDARD)
    var autoSave by mutableStateOf(false)
    var showTouches by mutableStateOf(false)
    var screenshotUsesLogcat by mutableStateOf(true)
    var destinationStatus by mutableStateOf("")
    var screenshotStatus by mutableStateOf("")
    var applicationStatus by mutableStateOf("")
    var captureActive by mutableStateOf(false)
    /** Last text sent to a destination check, so programmatic resets do not start a second check. */
    var checkedDestination: String? = null
    var checkedScreenshotDirectory: String? = null

    val controls: SettingsControls get() = settingsControls(appSelection, captureMode, screenshotUsesLogcat, captureActive)

    fun draft(): ReplaySettings = ReplaySettings(
        retentionSeconds = retention.text.toString().toIntOrNull() ?: 0,
        destination = destination.text.toString(),
        appSelection = appSelection,
        manualPackage = manualPackage.text.toString(),
        captureMode = captureMode,
        autoSaveOnCrash = autoSave,
        showTouches = showTouches,
        screenshotUsesLogcatDestination = screenshotUsesLogcat,
        screenshotDirectory = screenshotDirectory.text.toString(),
        videoQuality = videoQuality,
    )

    fun load(settings: ReplaySettings) {
        retention.setTextAndPlaceCursorAtEnd(settings.retentionSeconds.toString())
        destination.setTextAndPlaceCursorAtEnd(settings.destination)
        screenshotUsesLogcat = settings.screenshotUsesLogcatDestination
        screenshotDirectory.setTextAndPlaceCursorAtEnd(settings.screenshotDirectory)
        appSelection = settings.appSelection
        captureMode = settings.captureMode
        videoQuality = settings.videoQuality
        autoSave = settings.autoSaveOnCrash
        showTouches = settings.showTouches
        manualPackage.setTextAndPlaceCursorAtEnd(settings.manualPackage)
    }
}

/**
 * Owns an EDT form draft; only Apply persists it, and async checks are bound to form identity and revision.
 * [Configurable.NoScroll]: the Compose content scrolls itself, and a Settings scroll pane around a Compose panel
 * asks it for an unbounded preferred size that Compose cannot measure.
 */
class ReplaySettingsConfigurable(private val project: Project) : Configurable, Configurable.NoScroll {
    private var form: SettingsForm? = null
    private var screenshotValidation: DestinationValidation? = null
    private var screenshotValidationJob: Job? = null
    private var screenshotRevision = 0L
    private var uiLifetime: Disposable? = null
    private var validationJob: Job? = null
    private var destinationValidation: DestinationValidation? = null
    private var destinationRevision = 0L
    private var replayService: ReplayProjectService? = null

    override fun getDisplayName(): String = "Android Replay Buffer"

    /** Creates the draft and its subscriptions without any Swing or Compose component. */
    internal fun createForm(): SettingsForm {
        replayService = project.getService(ReplayProjectService::class.java)
        val pluginDirectory = (javaClass.classLoader as? PluginAwareClassLoader)?.pluginDescriptor?.pluginPath
        val created = SettingsForm(pluginDirectory?.let { ReplayCliRegistry.launcher(it).toString() })
        val lifetime = Disposer.newDisposable("Replay Buffer settings UI")
        uiLifetime = lifetime
        form = created
        Disposer.register(replayService ?: project, lifetime)
        Disposer.register(lifetime) { validationJob?.cancel(); screenshotValidationJob?.cancel() }
        project.messageBus.connect(lifetime).subscribe(ReplayProjectService.ENVIRONMENT_CHANGED, Runnable {
            ApplicationManager.getApplication().invokeLater({
                if (uiLifetime === lifetime && !project.isDisposed) renderApplication()
            }, ModalityState.any())
        })
        checkDestination()
        checkScreenshotDestination()
        renderApplication()
        return created
    }

    override fun createComponent(): JComponent {
        val created = createForm()
        return JewelComposePanel { SettingsContent(created, ::onFormEdited, ::chooseFolder) }
    }

    private fun onFormEdited() {
        val current = form ?: return
        if (current.destination.text.toString() != current.checkedDestination) checkDestination()
        val screenshotKey = "${current.screenshotUsesLogcat}:${current.screenshotDirectory.text}"
        if (screenshotKey != current.checkedScreenshotDirectory) checkScreenshotDestination()
    }

    private fun chooseFolder(target: TextFieldState) {
        val current = target.text.toString().takeIf { it.isNotEmpty() }?.let { LocalFileSystem.getInstance().findFileByPath(it) }
        FileChooser.chooseFile(FileChooserDescriptorFactory.createSingleFolderDescriptor(), project, current) { selected ->
            if (form != null) {
                target.setTextAndPlaceCursorAtEnd(selected.toNioPath().toString())
                onFormEdited()
            }
        }
    }

    private fun renderApplication() {
        val current = form ?: return
        val service = replayService
        val application = service?.environment?.application
        current.applicationStatus = "対象の解決結果: ${application?.packageName ?: application?.reason ?: "確認中…"}" +
            (application?.configurationName?.let { "（Run: $it）" } ?: "")
        current.captureActive = captureActive()
    }

    private fun captureActive(): Boolean {
        val service = replayService
        return store().enabled || service?.captureView?.snapshot?.enabled == true || service?.resolving == true || (service?.captureView?.pending ?: 0) > 0
    }

    private fun checkDestination() {
        val current = form ?: return
        val lifetime = uiLifetime ?: return
        val value = current.destination.text.toString()
        current.checkedDestination = value
        val revision = ++destinationRevision
        validationJob?.cancel()
        validationJob = null
        destinationValidation = null
        current.destinationStatus = if (value.isEmpty()) {
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
                current.destinationStatus = result.error.orEmpty()
            }
        }
    }

    private fun checkScreenshotDestination() {
        val current = form ?: return
        val lifetime = uiLifetime ?: return
        val followsLogcat = current.screenshotUsesLogcat
        current.checkedScreenshotDirectory = "$followsLogcat:${current.screenshotDirectory.text}"
        val revision = ++screenshotRevision
        screenshotValidationJob?.cancel()
        screenshotValidationJob = null
        screenshotValidation = null
        val location = if (followsLogcat) {
            try { LogcatScreenshotSettings.saveLocation() } catch (cancelled: com.intellij.openapi.progress.ProcessCanceledException) {
                throw cancelled
            } catch (_: RuntimeException) {
                current.screenshotStatus = LOGCAT_DESTINATION_UNAVAILABLE
                return
            } catch (_: LinkageError) {
                current.screenshotStatus = LOGCAT_DESTINATION_UNAVAILABLE
                return
            }
        } else null
        val value = location ?: current.screenshotDirectory.text.toString()
        current.screenshotStatus = if (!followsLogcat && value.isEmpty()) "スクリーンショット専用の保存先を指定してください。" else "保存先を確認中…"
        if (!followsLogcat && value.isEmpty()) {
            screenshotValidation = validateDestination(value)
            return
        }
        screenshotValidationJob = replayService?.scope?.checkDestination(value, {
            if (location == null) value else LogcatScreenshotSettings.resolveDirectory(project, location).toString()
        }) { result ->
            if (uiLifetime === lifetime && !project.isDisposed && revision == screenshotRevision) {
                screenshotValidation = result
                current.screenshotStatus = if (followsLogcat) {
                    "Logcatのスクショ保存先: ${result.destination}。" +
                        (result.error ?: "保存時の最新設定を使い、直下へPNG1枚を保存します。IDE設定の初期値はデスクトップです。")
                } else result.error ?: "専用フォルダ直下にPNG画像1枚を保存します。"
            }
        }
    }

    private fun draft(): ReplaySettings = form?.draft() ?: ReplaySettings()

    override fun isModified(): Boolean = form != null && draft() != store().settings()

    override fun apply() {
        if (form == null || project.isDisposed || !isModified()) return
        val settings = draft()
        val checked = destinationValidation?.takeIf { it.destination == settings.destination }
            ?: throw ConfigurationException("保存先を確認中です。確認が終わってから適用してください。")
        val screenshotChecked = if (settings.screenshotUsesLogcatDestination) DestinationValidation("", null) else {
            screenshotValidation?.takeIf { it.destination == settings.screenshotDirectory }
                ?: throw ConfigurationException("スクリーンショットの保存先を確認中です。確認が終わってから適用してください。")
        }
        settings.validationError(checked, screenshotChecked)?.let { throw ConfigurationException(it) }
        val service = replayService
        if ((settings.captureMode != store().settings().captureMode || settings.videoQuality != store().settings().videoQuality ||
            settings.showTouches != store().settings().showTouches) &&
            (store().enabled || service?.captureView?.snapshot?.enabled == true || service?.resolving == true || (service?.captureView?.pending ?: 0) > 0))
            throw ConfigurationException("取得をOFFにし、設定・操作の反映が終わってから取得モード・動画品質・タップ位置表示を変更してください。")
        try { store().apply(settings, checked, screenshotChecked) } catch (failure: IllegalArgumentException) {
            throw ConfigurationException(failure.message.orEmpty())
        }
        project.messageBus.syncPublisher(ReplaySettingsStore.CHANGED).run()
    }

    override fun reset() {
        val current = form ?: return
        if (project.isDisposed) return
        current.load(store().settings())
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
        replayService = null
        form = null
    }

    private fun store(): ReplaySettingsStore = project.getService(ReplaySettingsStore::class.java)
}

@Composable
private fun Hint(text: String) {
    if (text.isNotBlank()) Text(text, color = JewelTheme.globalColors.text.info)
}

@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
private fun WithTooltip(tooltip: String, content: @Composable () -> Unit) {
    Tooltip(tooltip = { Text(tooltip) }, content = content)
}

@Composable
private fun Labeled(label: String, content: @Composable () -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(label, modifier = Modifier.width(140.dp))
        content()
    }
}

@Composable
private fun FolderField(
    state: TextFieldState,
    name: String,
    enabled: Boolean,
    choose: (TextFieldState) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        TextField(state, modifier = Modifier.width(360.dp).semantics { contentDescription = name }, enabled = enabled)
        OutlinedButton({ choose(state) }, enabled = enabled,
            modifier = Modifier.semantics { contentDescription = "${name}を選ぶ" }) { Text("参照…") }
    }
}

@Composable
internal fun SettingsContent(form: SettingsForm, edited: () -> Unit, choose: (TextFieldState) -> Unit) {
    LaunchedEffect(form) {
        snapshotFlow { Triple(form.destination.text.toString(), form.screenshotDirectory.text.toString(), form.screenshotUsesLogcat) }
            .collect { edited() }
    }
    val controls = form.controls
    VerticallyScrollableContainer(modifier = Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxWidth().padding(end = 12.dp, bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            // 1. Basics, set once.
            GroupHeader("基本", modifier = Modifier.semantics { heading() })
            Labeled("動画・ログの保存先:") { FolderField(form.destination, "保存先フォルダ", true, choose) }
            Hint(form.destinationStatus)
            Labeled("保持時間:") {
                TextField(form.retention, modifier = Modifier.width(80.dp).semantics { contentDescription = "保持時間（1〜900秒）" })
                Text("秒（1〜900）")
            }
            Labeled("対象アプリ:") {
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    AppSelectionMode.entries.forEach { mode ->
                        RadioButtonRow(mode.toString(), form.appSelection == mode, { form.appSelection = mode })
                    }
                }
            }
            Labeled("package名:") {
                WithTooltip(PACKAGE_GUIDANCE) {
                    TextField(form.manualPackage, enabled = controls.manualPackageEnabled,
                        modifier = Modifier.width(280.dp).semantics { contentDescription = "対象アプリのpackage名" })
                }
            }
            Hint(form.applicationStatus)

            // 2. What is recorded; locked while capture runs.
            GroupHeader("記録する内容", modifier = Modifier.semantics { heading() })
            controls.recordingNotice?.let { Text(it, color = JewelTheme.globalColors.text.warning) }
            Labeled("取得モード:") {
                WithTooltip(CAPTURE_MODE_TOOLTIP) {
                    val modes = CaptureMode.entries
                    ListComboBox(modes.map(::captureModeDescription), modes.indexOf(form.captureMode), { form.captureMode = modes[it] },
                        modifier = Modifier.width(280.dp).semantics { contentDescription = "取得モード" }, enabled = controls.recordingEnabled)
                }
            }
            Labeled("動画品質:") {
                WithTooltip(VIDEO_QUALITY_TOOLTIP) {
                    val qualities = VideoQuality.entries
                    ListComboBox(qualities.map(VideoQuality::toString), qualities.indexOf(form.videoQuality), { form.videoQuality = qualities[it] },
                        modifier = Modifier.width(280.dp).semantics { contentDescription = "動画品質" }, enabled = controls.videoOptionsEnabled)
                }
            }
            WithTooltip(SHOW_TOUCHES_TOOLTIP) {
                CheckboxRow("取得中は端末画面にタップ位置を表示する（動画に映ります）", form.showTouches, { form.showTouches = it },
                    enabled = controls.videoOptionsEnabled, modifier = Modifier.semantics { contentDescription = "タップ位置の表示" })
            }

            // 3. Screenshot destination.
            GroupHeader("スクリーンショットの保存先", modifier = Modifier.semantics { heading() })
            WithTooltip(SCREENSHOT_LOGCAT_TOOLTIP) {
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    RadioButtonRow("Logcatのスクショ保存先に合わせる", form.screenshotUsesLogcat, { form.screenshotUsesLogcat = true },
                        modifier = Modifier.semantics { contentDescription = "スクリーンショットをLogcatのスクショ保存先へ保存" })
                    RadioButtonRow("手動で指定", !form.screenshotUsesLogcat, { form.screenshotUsesLogcat = false })
                }
            }
            if (controls.screenshotDirectoryEnabled) {
                Labeled("手動の保存先フォルダ:") {
                    FolderField(form.screenshotDirectory, "スクリーンショットの手動保存先", true, choose)
                }
            }
            Hint(form.screenshotStatus)

            // 4. Crash auto-save.
            GroupHeader("クラッシュ時の自動保存", modifier = Modifier.semantics { heading() })
            WithTooltip(AUTO_SAVE_TOOLTIP) {
                CheckboxRow("クラッシュ報告で自動保存する", form.autoSave, { form.autoSave = it },
                    modifier = Modifier.semantics { contentDescription = "クラッシュ報告の自動保存" })
            }
            Hint(AUTO_SAVE_SCOPE)

            // 5. Terminal and agents, collapsed.
            var cliExpanded by remember { mutableStateOf(false) }
            Text((if (cliExpanded) "▼ " else "▶ ") + "ターミナル・エージェントから操作",
                modifier = Modifier.clickable { cliExpanded = !cliExpanded }.semantics { heading() }, fontWeight = FontWeight.Bold)
            if (cliExpanded) {
                Hint(CLI_GUIDANCE)
                Labeled("CLIの場所:") {
                    val launcher = remember { TextFieldState(form.launcher ?: "CLIの場所を確認できません") }
                    TextField(launcher, readOnly = true, modifier = Modifier.width(360.dp).semantics { contentDescription = "CLIの場所" })
                    OutlinedButton({ form.launcher?.let { CopyPasteManager.getInstance().setContents(StringSelection(it)) } },
                        enabled = form.launcher != null, modifier = Modifier.semantics { contentDescription = "CLIの場所をコピー" }) { Text("コピー") }
                }
            }
            Hint(APPLY_GUIDANCE)
        }
    }
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

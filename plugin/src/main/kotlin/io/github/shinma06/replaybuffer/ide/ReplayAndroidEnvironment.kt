package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.android.tools.idea.run.AndroidRunConfigurationBase
import com.android.tools.idea.run.ApkProvisionException
import com.intellij.execution.RunManager
import com.intellij.openapi.application.readAction
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import io.github.shinma06.replaybuffer.settings.AppSelectionMode
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.settings.isApplicationId
import org.jetbrains.android.sdk.AndroidSdkUtils
import java.nio.file.Files
import java.nio.file.Path

/** IDE/model reading only; this boundary never launches adb or starts capture. */
object ReplayAndroidEnvironment {
    suspend fun resolve(project: Project, settings: ReplaySettings): AndroidReplayEnvironment {
        val model = readAction {
            if (project.isDisposed) return@readAction null
            val configuration = RunManager.getInstance(project).selectedConfiguration?.configuration
            val android = configuration as? AndroidRunConfigurationBase
            val sdk = AndroidSdkUtils.getFirstAndroidModuleSdkData(project)
                ?: AndroidSdkUtils.getProjectSdkData(project)
            val app = if (settings.appSelection == AppSelectionMode.MANUAL) {
                selectApplication(settings, configuration?.name, null, null)
            } else if (android == null) {
                selectApplication(settings, configuration?.name, null, "Androidの実行対象を選択してください。")
            } else if (DumbService.isDumb(project) || GradleSyncState.getInstance(project).isSyncInProgress) {
                selectApplication(settings, android.name, null, "Gradle同期またはindexingの完了を待っています。")
            } else if (GradleSyncState.getInstance(project).lastSyncFailed()) {
                selectApplication(settings, android.name, null, "Gradle同期に失敗しています。同期後に再確認します。")
            } else {
                try {
                    selectApplication(settings, android.name, android.applicationIdProvider?.packageName, null)
                } catch (_: ApkProvisionException) {
                    selectApplication(settings, android.name, null, "実行対象のapplicationIdを取得できません。Gradle同期を確認してください。")
                }
            }
            sdk?.location to app
        } ?: return AndroidReplayEnvironment(null, "projectは終了しています。", ApplicationSelection(null, null, "projectは終了しています。"))
        val adb = model.first?.let { sdkAdb(it, System.getProperty("os.name").startsWith("Windows")) }
            ?.takeIf { Files.isRegularFile(it) && Files.isExecutable(it) }
        return AndroidReplayEnvironment(adb, if (adb == null) "projectのAndroid SDKとplatform-toolsを確認してください。" else null, model.second)
    }
}

data class ApplicationSelection(val packageName: String?, val configurationName: String?, val reason: String?)

data class AndroidReplayEnvironment(val adb: Path?, val adbReason: String?, val application: ApplicationSelection)

internal fun sdkAdb(sdk: Path, windows: Boolean): Path =
    sdk.toAbsolutePath().normalize().resolve("platform-tools").resolve(if (windows) "adb.exe" else "adb")

internal fun selectApplication(
    settings: ReplaySettings,
    configurationName: String?,
    automaticPackage: String?,
    automaticReason: String?,
): ApplicationSelection {
    val packageName = if (settings.appSelection == AppSelectionMode.MANUAL) settings.manualPackage else automaticPackage
    return if (packageName != null && isApplicationId(packageName)) {
        ApplicationSelection(packageName, configurationName, null)
    } else {
        ApplicationSelection(null, configurationName, automaticReason ?: "設定で対象アプリのpackage名を指定してください。")
    }
}

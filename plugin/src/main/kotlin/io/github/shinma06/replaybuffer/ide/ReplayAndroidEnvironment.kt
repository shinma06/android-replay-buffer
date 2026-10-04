package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.android.tools.idea.run.AndroidRunConfigurationBase
import com.android.tools.idea.run.ApkProvisionException
import com.intellij.execution.RunManager
import com.intellij.openapi.application.readAction
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import io.github.shinma06.replaybuffer.settings.AppSelectionMode
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.settings.isApplicationId
import java.nio.file.Files
import java.nio.file.Path
import kotlinx.coroutines.CancellationException
import org.jetbrains.android.sdk.AndroidSdkUtils

internal const val MANUAL_APPLICATION_GUIDANCE =
    "設定（Tools → Android Replay Buffer）で対象アプリを「手動」に切り替え、package名を指定してください。"

/** IDE/model reading only; this boundary never launches adb or starts capture. */
object ReplayAndroidEnvironment {
    suspend fun resolve(project: Project, settings: ReplaySettings): AndroidReplayEnvironment {
        val model = readAction {
            if (project.isDisposed) return@readAction null
            val configuration = RunManager.getInstance(project).selectedConfiguration?.configuration
            val android = configuration as? AndroidRunConfigurationBase
            val sdk = AndroidSdkUtils.getFirstAndroidModuleSdkData(project)
                ?: AndroidSdkUtils.getProjectSdkData(project)
            val app = resolveApplication(configuration?.name) {
                if (settings.appSelection == AppSelectionMode.MANUAL) {
                    selectApplication(settings, configuration?.name, null, null)
                } else if (android == null) {
                    selectApplication(settings, configuration?.name, null, "Androidの実行対象を選択してください。")
                } else if (DumbService.isDumb(project) || GradleSyncState.getInstance(project).isSyncInProgress) {
                    selectApplication(settings, android.name, null, "Gradle同期またはindexingの完了を待っています。")
                } else if (GradleSyncState.getInstance(project).lastSyncFailed()) {
                    selectApplication(settings, android.name, null, "Gradle同期に失敗しています。同期後に再確認します。")
                } else {
                    selectApplication(settings, android.name, android.applicationIdProvider?.packageName, null)
                }
            }
            sdk?.location to app
        } ?: return AndroidReplayEnvironment(null, "projectは終了しています。", ApplicationSelection(null, null, "projectは終了しています。"))
        return sdkEnvironment(model.first, model.second)
    }
}

internal fun resolveApplication(configurationName: String?, resolve: () -> ApplicationSelection): ApplicationSelection = try {
    resolve()
} catch (cancelled: CancellationException) {
    throw cancelled
} catch (cancelled: ProcessCanceledException) {
    throw cancelled
} catch (_: ApkProvisionException) {
    ApplicationSelection(null, configurationName, "実行対象のapplicationIdを取得できません。Gradle同期を確認してください。 " +
        MANUAL_APPLICATION_GUIDANCE)
} catch (_: RuntimeException) {
    ApplicationSelection(null, configurationName, "対象アプリの情報を取得できません。Gradle同期を確認するか、" +
        MANUAL_APPLICATION_GUIDANCE)
}

internal fun sdkEnvironment(sdk: Path?, application: ApplicationSelection): AndroidReplayEnvironment {
    val adb = sdk?.let { sdkAdb(it, System.getProperty("os.name").startsWith("Windows")) }
        ?.takeIf { Files.isRegularFile(it) && Files.isExecutable(it) }
    return AndroidReplayEnvironment(adb, if (adb == null) "projectのAndroid SDKとplatform-toolsを確認してください。" else null, application)
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
        ApplicationSelection(null, configurationName,
            automaticReason?.let { "$it " }.orEmpty() + MANUAL_APPLICATION_GUIDANCE)
    }
}

package io.github.shinma06.arb.ide

import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Lightweight startup hook invoked off-EDT when a project opens:
 * - Launches the bundled `arb-tray` menu-bar / system-tray process when opted in via Settings
 *   ([ArbLocalSettingsState.trayEnabled], default `false`).
 * - Runs the isolated one-time legacy v1 settings migration ([LegacyV1SettingsMigrator]).
 */
class ArbStartupActivity : ProjectActivity {
    override suspend fun execute(project: Project) {
        withContext(Dispatchers.IO) {
            if (ArbLocalSettingsState.getInstanceOrNull()?.trayEnabled == true) {
                runCatching { ArbdDaemonService.getInstance().syncTrayState(true) }
            }
            LegacyV1SettingsMigrator.migrateOnceIfNeeded(project)
        }
    }
}

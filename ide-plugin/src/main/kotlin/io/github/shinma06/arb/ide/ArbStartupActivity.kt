package io.github.shinma06.arb.ide

import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Lightweight startup hook invoked off-EDT when a project opens:
 * - Starts the parent-bound `arbd --exit-on-stdin-eof` right away — the Tool Window does not have
 *   to be opened — so `capture_enabled` recording runs as soon as Android Studio starts, and the
 *   daemon stops when Studio exits ([ArbdDaemonService]). Idempotent across projects.
 * - Then launches the bundled `arb-tray --exit-on-stdin-eof` when opted in via Settings
 *   ([ArbLocalSettingsState.trayEnabled], default `false`).
 * - Runs the isolated one-time legacy v1 settings migration ([LegacyV1SettingsMigrator]).
 */
class ArbStartupActivity : ProjectActivity {
    override suspend fun execute(project: Project) {
        withContext(Dispatchers.IO) {
            ArbdDaemonService.getInstance().startParentBoundLifecycle {
                ArbLocalSettingsState.getInstanceOrNull()?.trayEnabled == true
            }
            LegacyV1SettingsMigrator.migrateOnceIfNeeded(project)
        }
    }
}

package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.google.gson.JsonParser
import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.notification.NotificationDisplayType
import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.client.ClientSession
import com.intellij.openapi.client.ClientSessionsManager
import com.intellij.openapi.util.Disposer
import io.github.shinma06.replaybuffer.cli.CliRequest
import io.github.shinma06.replaybuffer.cli.CliResponse
import io.github.shinma06.replaybuffer.cli.ReplayCli
import io.github.shinma06.replaybuffer.cli.ReplayCliServer
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.settings.ReplaySettingsState
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import java.io.ByteArrayOutputStream
import java.io.PrintStream
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.PosixFilePermissions
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.ValueSource
import org.junit.jupiter.api.io.TempDir

class ReplayInitializationCliTest {
    @TempDir lateinit var temporary: Path

    @ParameterizedTest
    @ValueSource(booleans = [false, true])
    fun `status and daemon on the same imported project preserve security and capture settings`(unsafe: Boolean) {
        val config = Files.createDirectory(temporary.resolve("config"))
        val projectPath = Files.createDirectory(temporary.resolve("project"))
        val outside = Files.createDirectory(temporary.resolve("outside"))
        val stable = prepareCleanupDirectory(config, projectPath.toString())
        val components = listOf(stable.parent.parent, stable.parent, stable)
        val imported = PosixFilePermissions.fromString("rwxr-xr-x")
        components.forEach { Files.setPosixFilePermissions(it, imported) }
        if (unsafe) {
            Files.delete(stable)
            Files.createSymbolicLink(stable, outside)
        }
        val lifetime = Disposer.newDisposable()
        val app = MockApplication(lifetime)
        ApplicationManager.setApplication(app, lifetime)
        app.registerService(CoroutineScope::class.java, app.getCoroutineScope())
        app.registerService(ClientSessionsManager::class.java, ClientSessionsManager<ClientSession>(app.getCoroutineScope()))
        app.registerService(NotificationGroupManager::class.java, object : NotificationGroupManager {
            override fun getNotificationGroup(id: String) =
                NotificationGroup.create(id, NotificationDisplayType.NONE, false, null, null, PluginId.getId("com.intellij"))
            override fun isGroupRegistered(id: String) = false
            override fun getRegisteredNotificationGroups() = emptyList<NotificationGroup>()
            override fun isRegisteredNotificationId(id: String) = false
        })
        val project = object : MockProject(null, lifetime) { override fun getBasePath() = projectPath.toString() }
        val store = ReplaySettingsStore()
        store.loadState(ReplaySettingsState(enabled = !unsafe, captureMode = CaptureMode.LOG_ONLY, retentionSeconds = 83))
        project.registerService(ReplaySettingsStore::class.java, store)
        project.registerService(GradleSyncState::class.java, Proxy.newProxyInstance(
            javaClass.classLoader, arrayOf(GradleSyncState::class.java),
        ) { _, method, args ->
            check(method.name == "subscribe")
            project.messageBus.connect(args[2] as Disposable)
        } as GradleSyncState)
        // Keep the real CLI handler and core, but never discover a host SDK, registry or IDE config.
        val service = ReplayProjectService(project, CoroutineScope(SupervisorJob().also { it.cancel() }))
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val capture = ReplayProjectCapture(scope, store, {}, {}) { prepareCleanupDirectory(config, projectPath.toString()) }
        val environment = AndroidReplayEnvironment(null, "SDK未設定", ApplicationSelection(null, null, "未選択"))
        fun inject(name: String, value: Any) = service.javaClass.getDeclaredField(name).apply { isAccessible = true }.set(service, value)
        inject("capture", capture)
        inject("resolving", false)
        inject("environment", environment)
        val handler = service.javaClass.getDeclaredMethod("executeCli", CliRequest::class.java).apply { isAccessible = true }
        val registry = temporary.resolve("registry")
        try {
            assertEquals(!unsafe, capture.applySettings(store.settings(), environment, 0).get(15, TimeUnit.SECONDS).accepted)
            ReplayCliServer(registry, projectPath, "build") { handler.invoke(service, it) as CliResponse }.use { server ->
                fun cli(command: String, expected: Int): String {
                    val output = ByteArrayOutputStream()
                    val error = ByteArrayOutputStream()
                    val code = ReplayCli.run(listOf("--project", projectPath.toString(), command),
                        PrintStream(output), PrintStream(error), registry) { "build" }
                    assertEquals(expected, code, error.toString())
                    return (if (expected == 0) output else error).toString().trim()
                }
                fun status() = JsonParser.parseString(cli("status", 0)).asJsonObject
                if (unsafe) {
                    val failed = status()
                    assertFalse(failed["running"].asBoolean)
                    val reason = failed["error"].asString
                    assertTrue(reason.contains(config.toRealPath().relativize(stable).toString()) && reason.contains("700"), reason)
                    assertTrue(cli("daemon", 1).contains(reason))
                    assertTrue(Files.isSymbolicLink(stable))
                    Files.delete(stable)
                    Files.createDirectory(stable)
                    Files.setPosixFilePermissions(stable, imported)
                    assertFalse(status()["running"].asBoolean)
                    assertEquals(imported, Files.getPosixFilePermissions(stable))
                } else assertTrue(status()["running"].asBoolean)
                assertEquals("started", cli("daemon", 0))
                val restored = status()
                assertTrue(restored["running"].asBoolean)
                assertEquals(null, capture.view.initializationError)
                assertEquals(capture.view.snapshot!!.error, restored["error"]?.takeUnless { it.isJsonNull }?.asString)
                for (component in components) assertEquals(PosixFilePermissions.fromString("rwx------"), Files.getPosixFilePermissions(component))
                assertEquals("LOG_ONLY", restored["capture_mode"].asString)
                assertEquals(83, restored["replay_seconds"].asInt)
                server.close()
                server.termination.get(5, TimeUnit.SECONDS)
            }
        } finally {
            service.dispose()
            capture.termination.get(15, TimeUnit.SECONDS)
            scope.cancel()
            Disposer.dispose(lifetime)
        }
    }
}

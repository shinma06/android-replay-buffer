package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.run.ApkProvisionException
import com.intellij.openapi.progress.ProcessCanceledException
import io.github.shinma06.replaybuffer.settings.AppSelectionMode
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import java.nio.file.Files
import java.nio.file.Path
import kotlinx.coroutines.CancellationException
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class ReplayAndroidEnvironmentTest {
    @TempDir lateinit var directory: Path

    @Test
    fun `application model failures retain the fresh SDK executable and cancellation is propagated`() {
        val tools = Files.createDirectories(directory.resolve("platform-tools"))
        val adb = Files.createFile(tools.resolve(if (System.getProperty("os.name").startsWith("Windows")) "adb.exe" else "adb"))
        adb.toFile().setExecutable(true)
        val missing = resolveApplication("Debug") { throw IllegalStateException("synthetic model unavailable") }
        val environment = sdkEnvironment(directory, missing)
        assertEquals(adb, environment.adb)
        assertNull(environment.adbReason)
        assertNull(environment.application.packageName)
        assertEquals("Debug", environment.application.configurationName)
        assertNotNull(environment.application.reason)
        assertNull(sdkEnvironment(directory.resolve("invalid-sdk"), missing).adb)
        for (cancelled in listOf(CancellationException("synthetic"), ProcessCanceledException())) {
            val thrown = assertThrows(cancelled.javaClass) { resolveApplication("Debug") { throw cancelled } }
            assertSame(cancelled, thrown)
        }
    }

    @Test
    fun `unresolved application reasons retain the cause and guide manual settings without losing SDK`() {
        val tools = Files.createDirectories(directory.resolve("platform-tools"))
        val adb = Files.createFile(tools.resolve(if (System.getProperty("os.name").startsWith("Windows")) "adb.exe" else "adb"))
        adb.toFile().setExecutable(true)
        for (reason in listOf(
            "Androidの実行対象を選択してください。",
            "Gradle同期またはindexingの完了を待っています。",
            "Gradle同期に失敗しています。同期後に再確認します。",
        )) {
            val application = selectApplication(ReplaySettings(), "Run", null, reason)
            assertNull(application.packageName)
            assertEquals("Run", application.configurationName)
            val message = application.reason!!
            assertTrue(message.startsWith(reason))
            assertTrue(message.contains("Tools → Android Replay Buffer"))
            assertTrue(message.contains("「手動」"))
            assertTrue(message.contains("package名"))
            assertEquals(adb, sdkEnvironment(directory, application).adb)
        }
        val provision = resolveApplication("Debug") { throw ApkProvisionException("synthetic unavailable") }
        assertNull(provision.packageName)
        assertEquals("Debug", provision.configurationName)
        val provisionMessage = provision.reason!!
        assertTrue(provisionMessage.startsWith("実行対象のapplicationIdを取得できません。Gradle同期を確認してください。"))
        assertTrue(provisionMessage.contains("Tools → Android Replay Buffer"))
        assertTrue(provisionMessage.contains("「手動」"))
        assertEquals(adb, sdkEnvironment(directory, provision).adb)
        val failure = resolveApplication("Debug") { throw IllegalStateException("synthetic") }
        val failureMessage = failure.reason!!
        assertTrue(failureMessage.startsWith("対象アプリの情報を取得できません。Gradle同期を確認するか、"))
        assertTrue(failureMessage.contains("Tools → Android Replay Buffer"))
        assertTrue(failureMessage.contains("「手動」"))
        assertEquals(adb, sdkEnvironment(directory, failure).adb)
        for (automaticPackage in listOf(null, "", "not a package", "com.app\nother")) {
            for (reason in listOf(null, "取得結果が不正です。")) {
                val missing = selectApplication(ReplaySettings(), "Run", automaticPackage, reason)
                assertNull(missing.packageName)
                assertEquals("Run", missing.configurationName)
                val message = missing.reason!!
                if (reason != null) assertTrue(message.startsWith(reason))
                assertTrue(message.contains("Tools → Android Replay Buffer"))
                assertTrue(message.contains("「手動」"))
                assertTrue(message.contains("package名"))
                assertEquals(adb, sdkEnvironment(directory, missing).adb)
            }
        }
        assertNull(selectApplication(ReplaySettings(), "Run", "com.example.ready", "not ready").reason)
    }

    @Test
    fun `manual selection wins over run changes until automatic mode is applied`() {
        val manual = ReplaySettings(appSelection = AppSelectionMode.MANUAL, manualPackage = "com.example.manual")
        for (automatic in listOf("com.example.debug", "com.other.release", null, "not a package")) {
            val selected = selectApplication(manual, "Run", automatic, "not ready")
            assertEquals("com.example.manual", selected.packageName)
            assertNull(selected.reason)
        }
        val automatic = manual.copy(appSelection = AppSelectionMode.AUTOMATIC)
        assertEquals("com.example.debug", selectApplication(automatic, "Debug", "com.example.debug", null).packageName)
        val missing = selectApplication(automatic, "Gradle", null, "Androidの実行対象を選択してください。")
        assertNull(missing.packageName)
        assertNotNull(missing.reason)
    }

    @Test
    fun `SDK executable is resolved under supplied SDK on every OS without PATH lookup`() {
        val sdk = Path.of(System.getProperty("java.io.tmpdir"), "sdk with spaces").toAbsolutePath()
        assertEquals(sdk.resolve("platform-tools/adb"), sdkAdb(sdk, false))
        assertEquals(sdk.resolve("platform-tools/adb.exe"), sdkAdb(sdk, true))
        val invalid = selectApplication(ReplaySettings(), "Run", "com.app\nother", null)
        assertNull(invalid.packageName)
        assertNotNull(invalid.reason)
    }
}

package io.github.shinma06.replaybuffer.ide

import io.github.shinma06.replaybuffer.settings.AppSelectionMode
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test
import java.nio.file.Path

class ReplayAndroidEnvironmentTest {
    @Test
    fun `manual selection wins over run changes until automatic mode is applied`() {
        val manual = ReplaySettings(appSelection = AppSelectionMode.MANUAL, manualPackage = "com.example.manual")
        for (automatic in listOf("com.example.debug", "com.other.release", null)) {
            assertEquals("com.example.manual", selectApplication(manual, "Run", automatic, "not ready").packageName)
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

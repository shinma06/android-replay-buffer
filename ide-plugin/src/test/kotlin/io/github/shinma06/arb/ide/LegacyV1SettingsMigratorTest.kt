package io.github.shinma06.arb.ide

import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.StringPatch
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class LegacyV1SettingsMigratorTest {
    private fun writeWorkspaceXml(projectDir: Path, content: String): Path {
        val ideaDir = Files.createDirectories(projectDir.resolve(".idea"))
        val xmlPath = ideaDir.resolve("workspace.xml")
        Files.writeString(xmlPath, content)
        return xmlPath
    }

    @Test
    fun concurrentProjectStartupAppliesLegacySettingsOnce(@TempDir tmp: Path) {
        writeWorkspaceXml(tmp, """<project><component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore"/></project>""")
        val settings = ArbLocalSettingsState()
        val pool = Executors.newFixedThreadPool(2)
        val entered = CountDownLatch(1)
        val secondEntered = CountDownLatch(1)
        val updates = AtomicInteger()
        fun migrate() = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, settings) {
            if (updates.incrementAndGet() == 1) {
                entered.countDown()
                secondEntered.await(1, TimeUnit.SECONDS)
            } else {
                secondEntered.countDown()
            }
            ArbConfig()
        }
        try {
            val first = pool.submit { migrate() }
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            val second = pool.submit { migrate() }
            first.get(5, TimeUnit.SECONDS)
            second.get(5, TimeUnit.SECONDS)
            assertEquals(1, updates.get())
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun migratesPopulatedV1WorkspaceXmlReadOnlyAndIgnoresDroppedFields(@TempDir tmp: Path) {
        val destDir = tmp.resolve("legacy-replays").toAbsolutePath().normalize()
        val shotDir = Path.of(System.getProperty("user.home"), "legacy-shots").toAbsolutePath().normalize()
        val xmlContent = """
            <?xml version="1.0" encoding="UTF-8"?>
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="enabled" value="true" />
                <option name="retentionSeconds" value="240" />
                <option name="destination" value="${'$'}PROJECT_DIR${'$'}/legacy-replays" />
                <option name="captureMode" value="LOG_ONLY" />
                <option name="autoSaveOnCrash" value="true" />
                <option name="screenshotUsesCommonDestination" value="false" />
                <option name="screenshotDirectory" value="${'$'}USER_HOME${'$'}/legacy-shots" />
                <option name="videoQuality" value="LIGHTWEIGHT" />
                <option name="recordAudio" value="true" />
                <option name="deviceSerial" value="[::1]:5555" />
                <option name="appSelection" value="MANUAL" />
                <option name="manualPackage" value="com.example.legacy" />
                <option name="showTouches" value="true" />
                <option name="tapDisplay" value="DEVICE" />
              </component>
            </project>
        """.trimIndent()
        val xmlPath = writeWorkspaceXml(tmp, xmlContent)
        val localSettings = ArbLocalSettingsState()
        val appliedPatches = mutableListOf<ArbConfigPatch>()

        val result = LegacyV1SettingsMigrator.migrateOnceIfNeeded(
            projectBasePath = tmp,
            localSettings = localSettings,
            configUpdater = { patch ->
                appliedPatches += patch
                ArbConfig()
            },
        )

        val migrated = assertIs<LegacyV1SettingsMigrator.MigrationResult.Migrated>(result)
        assertEquals(1, appliedPatches.size)
        val patch = assertNotNull(migrated.appliedDaemonPatch)
        assertEquals(240, patch.saveWindowSecs)
        assertEquals(OutputDirPatch.Set(destDir.toString()), patch.outputDirOverride)
        assertEquals(CaptureMode.LOGS_ONLY, patch.captureMode)
        assertEquals(1024, patch.videoMaxSize)
        assertEquals(15, patch.videoMaxFps)
        assertEquals(true, patch.audioEnabled)
        assertEquals(true, patch.crashAutosaveEnabled)
        assertEquals(true, patch.captureEnabled)
        assertEquals(StringPatch.Set("[::1]:5555"), patch.deviceSerial)

        assertFalse(localSettings.screenshotInheritLogcat)
        assertEquals(shotDir.toString(), localSettings.customScreenshotDir)
        assertTrue(localSettings.migratedFromV1)

        // Strictly read-only on .idea/workspace.xml.
        assertEquals(xmlContent, Files.readString(xmlPath))
    }

    @Test
    fun clampsOutOfRangeRetentionSecondsAndRejectsRelativeOrUnsafePaths(@TempDir tmp: Path) {
        val lowDir = Files.createDirectories(tmp.resolve("low"))
        writeWorkspaceXml(
            lowDir,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="retentionSeconds" value="1" />
                <option name="destination" value="relative/path/not-allowed" />
                <option name="screenshotDirectory" value="relative/shots" />
                <option name="deviceSerial" value="bad serial; rm -rf" />
                <option name="videoQuality" value="STANDARD" />
              </component>
            </project>
            """.trimIndent(),
        )
        val lowLocal = ArbLocalSettingsState()
        var lowPatch: ArbConfigPatch? = null
        val lowRes = LegacyV1SettingsMigrator.migrateOnceIfNeeded(lowDir, lowLocal) { patch ->
            lowPatch = patch
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.Migrated>(lowRes)
        assertEquals(ArbConfig.MIN_SAVE_WINDOW_SECS, lowPatch?.saveWindowSecs)
        assertEquals(OutputDirPatch.Unchanged, lowPatch?.outputDirOverride)
        assertEquals(StringPatch.Unchanged, lowPatch?.deviceSerial)
        assertEquals(1920, lowPatch?.videoMaxSize)
        assertEquals(30, lowPatch?.videoMaxFps)
        assertEquals("", lowLocal.customScreenshotDir)

        val highDir = Files.createDirectories(tmp.resolve("high"))
        writeWorkspaceXml(
            highDir,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="retentionSeconds" value="900" />
              </component>
            </project>
            """.trimIndent(),
        )
        val highLocal = ArbLocalSettingsState()
        var highPatch: ArbConfigPatch? = null
        val highRes = LegacyV1SettingsMigrator.migrateOnceIfNeeded(highDir, highLocal) { patch ->
            highPatch = patch
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.Migrated>(highRes)
        assertEquals(ArbConfig.MAX_SAVE_WINDOW_SECS, highPatch?.saveWindowSecs)
    }

    @Test
    fun returnsNoLegacySettingsFoundWhenXmlOrComponentIsAbsent(@TempDir tmp: Path) {
        val localSettings = ArbLocalSettingsState()
        var updateCalls = 0
        val missingRes = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) {
            updateCalls++
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.NoLegacySettingsFound>(missingRes)
        assertEquals(0, updateCalls)
        assertFalse(localSettings.migratedFromV1)

        writeWorkspaceXml(
            tmp,
            """
            <project version="4">
              <component name="OtherComponent">
                <option name="foo" value="bar" />
              </component>
            </project>
            """.trimIndent(),
        )
        val absentCompRes = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) {
            updateCalls++
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.NoLegacySettingsFound>(absentCompRes)
        assertEquals(0, updateCalls)
        assertFalse(localSettings.migratedFromV1)
    }

    @Test
    fun secondRunIsIdempotentAndNeverOverwritesSubsequentUserSettings(@TempDir tmp: Path) {
        writeWorkspaceXml(
            tmp,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="retentionSeconds" value="90" />
                <option name="screenshotUsesCommonDestination" value="false" />
              </component>
            </project>
            """.trimIndent(),
        )
        val localSettings = ArbLocalSettingsState()
        var updateCalls = 0
        val first = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) {
            updateCalls++
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.Migrated>(first)
        assertEquals(1, updateCalls)
        assertFalse(localSettings.screenshotInheritLogcat)
        assertTrue(localSettings.migratedFromV1)

        // User changes settings after migration; next launch must NEVER overwrite them.
        localSettings.screenshotInheritLogcat = true
        val second = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) {
            updateCalls++
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.AlreadyMigrated>(second)
        assertEquals(1, updateCalls)
        assertTrue(localSettings.screenshotInheritLogcat)
    }

    @Test
    fun malformedXmlAndXxeDoctypeFailSafelyWithoutMutatingSettings(@TempDir tmp: Path) {
        val malformedDir = Files.createDirectories(tmp.resolve("malformed"))
        writeWorkspaceXml(
            malformedDir,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="retentionSeconds" value="120"
            """.trimIndent(),
        )
        val local1 = ArbLocalSettingsState()
        var calls = 0
        val res1 = LegacyV1SettingsMigrator.migrateOnceIfNeeded(malformedDir, local1) {
            calls++
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.MalformedXml>(res1)
        assertEquals(0, calls)
        assertFalse(local1.migratedFromV1)

        // XXE DOCTYPE must be rejected cleanly by DocumentBuilderFactory disallow-doctype-decl.
        val xxeDir = Files.createDirectories(tmp.resolve("xxe"))
        writeWorkspaceXml(
            xxeDir,
            """
            <?xml version="1.0" encoding="UTF-8"?>
            <!DOCTYPE project [<!ENTITY secret SYSTEM "file:///etc/passwd">]>
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="destination" value="&secret;" />
              </component>
            </project>
            """.trimIndent(),
        )
        val local2 = ArbLocalSettingsState()
        val res2 = LegacyV1SettingsMigrator.migrateOnceIfNeeded(xxeDir, local2) {
            calls++
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.MalformedXml>(res2)
        assertEquals(0, calls)
        assertFalse(local2.migratedFromV1)
    }

    @Test
    fun daemonUnavailableDefersMigrationUntilNextLaunch(@TempDir tmp: Path) {
        writeWorkspaceXml(
            tmp,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="retentionSeconds" value="150" />
              </component>
            </project>
            """.trimIndent(),
        )
        val localSettings = ArbLocalSettingsState()
        val failed = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) {
            throw IOException("daemon offline")
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.DaemonUnavailable>(failed)
        assertFalse(localSettings.migratedFromV1, "must retry on next launch when daemon was offline")

        var appliedPatch: ArbConfigPatch? = null
        val retried = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) { patch ->
            appliedPatch = patch
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.Migrated>(retried)
        assertEquals(150, appliedPatch?.saveWindowSecs)
        assertTrue(localSettings.migratedFromV1)
        // Options omitted by IntelliJ's XmlSerializer carry the v1 default, not "unchanged".
        assertEquals(CaptureMode.VIDEO_AND_LOGS, appliedPatch?.captureMode)
    }

    @Test
    fun omittedV1OptionsMigrateAsV1DefaultsNotArbdDefaults(@TempDir tmp: Path) {
        // XmlSerializer drops options equal to their defaults: a user who only changed the
        // destination had v1 recordAudio=false, autoSaveOnCrash=false, enabled=false, 180 s.
        val destDir = tmp.resolve("only-dest").toAbsolutePath().normalize()
        writeWorkspaceXml(
            tmp,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="destination" value="$destDir" />
              </component>
            </project>
            """.trimIndent(),
        )
        val localSettings = ArbLocalSettingsState()
        localSettings.screenshotInheritLogcat = false
        var applied: ArbConfigPatch? = null
        val res = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) { patch ->
            applied = patch
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.Migrated>(res)
        val patch = assertNotNull(applied)
        assertEquals(false, patch.audioEnabled, "v1 recordAudio default is false")
        assertEquals(false, patch.crashAutosaveEnabled, "v1 autoSaveOnCrash default is false")
        assertEquals(false, patch.captureEnabled, "v1 enabled default is false")
        assertEquals(180, patch.saveWindowSecs, "v1 retentionSeconds default is 180")
        assertEquals(CaptureMode.VIDEO_AND_LOGS, patch.captureMode)
        assertEquals(1920, patch.videoMaxSize)
        assertEquals(30, patch.videoMaxFps)
        assertEquals(OutputDirPatch.Set(destDir.toString()), patch.outputDirOverride)
        assertTrue(localSettings.screenshotInheritLogcat, "v1 screenshotUsesCommonDestination default is true")
    }

    @Test
    fun recognizesV1VideoQualityAsSerializedByXmlSerializer(@TempDir tmp: Path) {
        // v1 VideoQuality overrides toString(), and that label is what lands in workspace.xml.
        writeWorkspaceXml(
            tmp,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="videoQuality" value="軽量（1024px / 15fps / 2Mbps）" />
              </component>
            </project>
            """.trimIndent(),
        )
        var applied: ArbConfigPatch? = null
        LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, ArbLocalSettingsState()) { patch ->
            applied = patch
            ArbConfig()
        }
        assertEquals(1024, applied?.videoMaxSize)
        assertEquals(15, applied?.videoMaxFps)
        assertEquals(1920 to 30, LegacyV1SettingsMigrator.parseV1VideoQuality("標準（1920px / 30fps / 8Mbps）"))
        assertEquals(null to null, LegacyV1SettingsMigrator.parseV1VideoQuality("unknown"))
    }

    @Test
    fun configWriteFailureLeavesMigrationPending(@TempDir tmp: Path) {
        writeWorkspaceXml(tmp, """<project><component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore"/></project>""")
        val settings = ArbLocalSettingsState()
        var writes = 0
        val result = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, settings) {
            if (++writes == 1) throw ArbdRpcException(-32603, "config write failed", "config.update")
            ArbConfig()
        }
        assertIs<LegacyV1SettingsMigrator.MigrationResult.DaemonUnavailable>(result)
        assertEquals(1, writes)
        assertFalse(settings.migratedFromV1)
    }

    @Test
    fun arbdWithoutCaptureEnabledStillMigratesTheRest(@TempDir tmp: Path) {
        val destDir = tmp.resolve("dest").toAbsolutePath().normalize()
        writeWorkspaceXml(
            tmp,
            """
            <project version="4">
              <component name="io.github.shinma06.replaybuffer.settings.ReplaySettingsStore">
                <option name="enabled" value="true" />
                <option name="destination" value="$destDir" />
                <option name="screenshotUsesCommonDestination" value="false" />
              </component>
            </project>
            """.trimIndent(),
        )
        val localSettings = ArbLocalSettingsState()
        val attempts = mutableListOf<ArbConfigPatch>()
        val res = LegacyV1SettingsMigrator.migrateOnceIfNeeded(tmp, localSettings) { patch ->
            attempts += patch
            if (patch.captureEnabled != null) {
                // Pre-T-7A arbd: ArbConfigPatch has deny_unknown_fields.
                throw ArbdRpcException(code = -32602, message = "unknown field `capture_enabled`", method = "config.update")
            }
            ArbConfig()
        }
        val migrated = assertIs<LegacyV1SettingsMigrator.MigrationResult.Migrated>(res)
        assertEquals(2, attempts.size)
        assertEquals(true, attempts[0].captureEnabled)
        assertNull(attempts[1].captureEnabled)
        assertEquals(OutputDirPatch.Set(destDir.toString()), attempts[1].outputDirOverride)
        assertNull(migrated.appliedDaemonPatch?.captureEnabled)
        assertFalse(localSettings.screenshotInheritLogcat)
        assertTrue(localSettings.migratedFromV1)
    }
}

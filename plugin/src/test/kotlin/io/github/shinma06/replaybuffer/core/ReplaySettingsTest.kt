package io.github.shinma06.replaybuffer.core

import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull

class ReplaySettingsTest {
    @Test
    fun unresolvedSdkAndSaveFolderRemainValidAppliedSettings() {
        val settings = ReplaySettings().validate()
        assertNull(settings.adbPath)
        assertNull(settings.saveDirectory)
        assertEquals(180, settings.replaySeconds)
        assertEquals(900, ReplaySettings.MAX_REPLAY_SECONDS)
    }

    @Test
    fun invalidInputIsRejectedBeforeExternalOperations() {
        for (seconds in listOf(0, 901)) {
            assertFailsWith<IllegalArgumentException> { ReplaySettings(replaySeconds = seconds).validate() }
        }
        assertFailsWith<IllegalArgumentException> { ReplaySettings(saveDirectory = Path.of("relative")).validate() }
        for (name in listOf("single", "com.1name", "com.app;rm", "com..app", "com.app\n")) {
            assertFailsWith<IllegalArgumentException> { ApplicationTarget(name).validate() }
        }
        assertFailsWith<IllegalArgumentException> { ApplicationTarget(mode = ApplicationMode.MANUAL).validate() }
        ApplicationTarget("com.example.app_1", ApplicationMode.MANUAL).validate()
    }
}

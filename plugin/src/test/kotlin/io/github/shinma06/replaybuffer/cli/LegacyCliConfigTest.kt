package io.github.shinma06.replaybuffer.cli

import java.nio.file.Path
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import org.junit.jupiter.api.Test

class LegacyCliConfigTest {
    private val cwd = Path.of("/working directory")
    private val home = Path.of("/home/owner")

    @Test fun `explicit legacy defaults and relative paths retain their meaning`() {
        val defaults = LegacyCliConfig.parse("{}", false, cwd, home)
        assertEquals(60, defaults.seconds)
        assertEquals("/home/owner/Replays", defaults.destination)
        val migrated = LegacyCliConfig.parse("""{"replay_seconds":90,"output_directory":"some recordings"}""", false, cwd, home)
        assertEquals(90, migrated.seconds)
        assertEquals("/working directory/some recordings", migrated.destination)
    }

    @Test fun `simple YAML and tilde keep legacy scalar meaning`() {
        val text = """
            # legacy
            replay_seconds: 15
            output_directory: "~/My Replays"
            segment_seconds: 5
            state_directory: ~/.replay-buffer
            scrcpy_path: /old/scrcpy
            ffmpeg_path: /old/ffmpeg
        """.trimIndent()
        val config = LegacyCliConfig.parse(text, true, cwd, home)
        assertEquals(15, config.seconds)
        assertEquals("/home/owner/My Replays", config.destination)
        assertEquals(config, LegacyCliConfig.parse(text.prependIndent("  "), true, cwd, home))
        for (key in listOf("segment_seconds", "state_directory", "scrcpy_path", "ffmpeg_path")) assertTrue(config.warning.contains(key))
    }

    @Test fun `nested YAML cannot promote child values into shared settings`() {
        for (child in listOf("output_directory: /existing/output", "replay_seconds: 15")) {
            for (indent in listOf("  ", "\t")) {
                assertFailsWith<IllegalArgumentException> {
                    LegacyCliConfig.parse("state_directory:\n$indent$child", true, cwd, home)
                }
            }
        }
    }

    @Test fun `malformed unknown duplicate nested and out of range settings fail before migration`() {
        val invalid = listOf("""{"replay_seconds":null}""", """{"segment_seconds":null}""", """{"adb_path":null}""",
            "[]", "{} {}", "{", "{\"replay_seconds\":1,\"replay_seconds\":2}",
            "{\"replay_seconds\":0}", "{\"replay_seconds\":901}", "{\"replay_seconds\":1.5}",
            "{\"output_directory\":{}}", "{\"output_directory\":true}", "{\"extra\":1}",
            "{\"adb_path\":\"/other/adb\"}", "{\"output_directory\":\"~someone/data\"}", "{\"segment_seconds\":0}")
        for (text in invalid) assertFailsWith<IllegalArgumentException>(text) { LegacyCliConfig.parse(text, false, cwd, home) }
        for (text in listOf("missing-colon", "output_directory: 'unclosed", "replay_seconds: 1\nreplay_seconds: 2")) {
            assertFailsWith<IllegalArgumentException>(text) { LegacyCliConfig.parse(text, true, cwd, home) }
        }
        assertFailsWith<IllegalArgumentException> { LegacyCliConfig.parse(" ".repeat(65537), false, cwd, home) }
    }
}

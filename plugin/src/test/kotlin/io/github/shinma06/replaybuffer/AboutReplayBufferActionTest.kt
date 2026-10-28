package io.github.shinma06.replaybuffer

import io.github.shinma06.replaybuffer.core.CaptureResources
import java.io.IOException
import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class AboutReplayBufferActionTest {
    @Test
    fun formalVersionDoesNotHidePackagedFullSource() {
        val message = aboutMessage("0.2.0") { CaptureResources.identity() }

        assertTrue(message.contains("バージョン: 0.2.0\n"))
        assertTrue(message.endsWith("ソースSHA: ${System.getProperty("replay.expected.source")}"))
        assertTrue(message.contains("直前の画面・logcatを保存できます。"))
        assertTrue(message.contains("Settings → Tools → Android Replay Buffer"))
        assertTrue(message.contains("初回は取得が無効です。"))
    }

    @Test
    fun missingOrMalformedSourceStaysUnknown() {
        for (source in listOf(null, "", "e22cac5", "G".repeat(40), "a".repeat(40) + "\n")) {
            val message = aboutMessage("0.2.0") {
                if (source == null) emptyMap() else mapOf("source" to source)
            }

            assertTrue(message.contains("バージョン: 0.2.0\n"))
            assertTrue(message.endsWith("ソースSHA: 不明（同梱source SHAを確認できません）"))
        }
    }

    @Test
    fun unreadableIdentityPreservesVersionAndDoesNotExposeExceptionDetails() {
        val failures = listOf(
            IOException("private identity detail"),
            IllegalArgumentException("private identity detail"),
            IllegalStateException("private identity detail"),
        )
        for (failure in failures) {
            val message = aboutMessage("0.2.0") { throw failure }

            assertTrue(message.contains("バージョン: 0.2.0\n"))
            assertTrue(message.endsWith("ソースSHA: 不明（同梱情報を読み取れません）"))
            assertFalse(message.contains("private identity detail"))
        }
    }
}

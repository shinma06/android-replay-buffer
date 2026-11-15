package io.github.shinma06.replaybuffer.ide

import io.github.shinma06.replaybuffer.core.SaveCancelled
import io.github.shinma06.replaybuffer.core.SaveFailure
import io.github.shinma06.replaybuffer.core.ScreenshotSaveFailure
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.file.Path
import java.util.concurrent.CompletionException

class ReplayOperationFailureTest {
    private val retained = Path.of(".trim A & B.partial")
    private val escaped = retained.toString().replace("&", "&amp;")

    @Test
    fun screenshotCliFailureKeepsTheReasonAndRetainedPathWithoutChangingResponseShape() {
        val response = screenshotCliFailure(ScreenshotSaveFailure("静止画の保存に失敗しました。", retained))
        assertEquals(1, response.code)
        assertEquals("", response.output)
        assertEquals("静止画の保存に失敗しました。\n保全した出力: $retained", response.error)
        assertEquals("失敗", screenshotCliFailure(ScreenshotSaveFailure("失敗")).error)
        assertEquals("スクリーンショットを保存できません。", screenshotCliFailure(null).error)
    }

    @Test
    fun trimFailureIncludesTheRetainedPathWithHtmlEscaping() {
        val cause = SaveFailure(retained, "容量不足 <full> & 書込失敗")
        for (failure in listOf(cause, CompletionException(cause))) {
            assertEquals("容量不足 &lt;full&gt; &amp; 書込失敗\n未完成の出力は保全します: $escaped",
                operationFailureContent(failure))
        }
    }

    @Test
    fun existingCancellationAndScreenshotFailuresKeepTheRetainedPath() {
        for (failure in listOf(SaveCancelled(retained), ScreenshotSaveFailure("失敗", retained))) {
            assertTrue(operationFailureContent(failure).contains("\n未完成の出力は保全します: $escaped"))
        }
    }

    @Test
    fun noRetainedOutputDoesNotInventAPath() {
        assertEquals("容量不足", operationFailureContent(SaveFailure(null, "容量不足")))
        assertEquals("保存先・空き容量・アクセス権を確認してください。", operationFailureContent(IllegalStateException()))
    }
}

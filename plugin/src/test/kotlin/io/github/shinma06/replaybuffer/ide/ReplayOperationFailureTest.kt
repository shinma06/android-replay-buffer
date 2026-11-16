package io.github.shinma06.replaybuffer.ide

import io.github.shinma06.replaybuffer.core.SaveCancelled
import io.github.shinma06.replaybuffer.core.SaveFailure
import io.github.shinma06.replaybuffer.core.ScreenshotSaveFailure
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test
import java.nio.file.Path
import java.util.concurrent.CompletionException

class ReplayOperationFailureTest {
    private val retained = Path.of(".trim A & B.partial")
    private val escaped = retained.toString().replace("&", "&amp;")

    @Test
    fun frameCliReportsOnlyKnownDestinationAndDecodeReasons() {
        for (reason in listOf(io.github.shinma06.replaybuffer.settings.LOGCAT_DESTINATION_UNAVAILABLE,
            "表示frameの復号に対応できません", "表示frameの復号に失敗しました")) {
            val response = frameCliResponse { throw IllegalStateException(reason, IllegalArgumentException("decoder")) }
            assertEquals(1, response.code)
            assertEquals("", response.output)
            assertEquals(reason, response.error)
        }
        val output = Path.of("frame.png")
        val response = frameCliResponse { output }
        assertEquals(0, response.code)
        assertEquals(output.toString(), response.output)
        assertEquals("", response.error)
    }

    @Test
    fun frameCliLeavesUnknownFailuresAndScreenshotRetentionForTheExistingHandler() {
        for (failure in listOf(IllegalStateException("unknown"), IllegalStateException(),
            ScreenshotSaveFailure("失敗", retained), CompletionException(IllegalStateException("表示frameの復号に失敗しました")))) {
            assertSame(failure, assertThrows(Exception::class.java) { frameCliResponse { throw failure } })
        }
    }

    @Test
    fun frameCliDoesNotTranslateCancellationInterruptionOrTimeoutEvenWhenWrapped() {
        for (failure in listOf(SaveCancelled(retained), java.util.concurrent.CancellationException("表示frameの復号に失敗しました"),
            com.intellij.openapi.progress.ProcessCanceledException(), InterruptedException(), java.io.InterruptedIOException(),
            java.nio.channels.ClosedByInterruptException(), java.util.concurrent.TimeoutException())) {
            val wrapped = IllegalStateException("表示frameの復号に失敗しました", CompletionException(failure))
            for (cause in listOf(failure, wrapped)) {
                assertSame(cause, assertThrows(Exception::class.java) { frameCliResponse { throw cause } })
            }
        }
    }

    @Test
    fun frameCliKeepsInterruptedAndCyclicFailuresOnTheExistingPath() {
        val failure = IllegalStateException("表示frameの復号に失敗しました")
        Thread.currentThread().interrupt()
        try {
            assertSame(failure, assertThrows(IllegalStateException::class.java) { frameCliResponse { throw failure } })
            assertTrue(Thread.currentThread().isInterrupted)
        } finally { Thread.interrupted() }
        val nested = IllegalStateException("nested", failure)
        failure.initCause(nested)
        assertSame(failure, assertThrows(IllegalStateException::class.java) { frameCliResponse { throw failure } })
    }

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

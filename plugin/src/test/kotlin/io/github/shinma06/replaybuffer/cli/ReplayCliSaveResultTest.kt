package io.github.shinma06.replaybuffer.cli

import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveAttempt
import io.github.shinma06.replaybuffer.core.SaveOutput
import io.github.shinma06.replaybuffer.core.SaveFailure
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import org.junit.jupiter.api.Test

class ReplayCliSaveResultTest {
    private fun deadline() = System.nanoTime() + TimeUnit.SECONDS.toNanos(1)

    @Test fun `completed path is stdout and partial capture warning is stderr`() {
        val attempt = SaveAttempt(SaveSnapshot(SavePhase.WRITING, "request", missingKinds = listOf("video")))
        attempt.complete(Result.success(SaveOutput(Path.of("/completed folder"), emptyList())))
        val result = cliSaveResponse("request", attempt.completion, deadline(), "legacy warning")
        assertEquals(0, result.code)
        assertEquals("/completed folder", result.output)
        assertTrue(result.error.contains("video"))
        assertTrue(result.error.contains("legacy warning"))
    }

    @Test fun `failed attempt never prints its intended destination as success`() {
        val attempt = SaveAttempt(SaveSnapshot(SavePhase.WRITING, "request", directory = Path.of("/intended"), attempt = 3))
        attempt.complete(Result.failure(SaveFailure(Path.of("/intended/attempt.partial"), "保存失敗")))
        val result = cliSaveResponse("request", attempt.completion, deadline(), "")
        assertEquals(1, result.code)
        assertEquals("", result.output)
        assertTrue(result.error.contains("保存失敗"))
        assertTrue(result.error.contains("attempt: 3"))
        assertTrue(result.error.contains("/intended/attempt.partial"))
    }

    @Test fun `timeout does not cancel the writer and unrelated results cannot become success`() {
        val pending = CompletableFuture<SaveSnapshot>()
        assertFailsWith<TimeoutException> { cliSaveResponse("request", pending.minimalCompletionStage(), System.nanoTime(), "") }
        assertFalse(pending.isDone)
        val saved = SaveSnapshot(SavePhase.COMPLETED, "request", directory = Path.of("/completed"))
        pending.complete(saved)
        assertEquals(0, cliSaveResponse("request", pending.minimalCompletionStage(), deadline(), "").code)
        assertFailsWith<IllegalStateException> { cliSaveResponse("other", pending.minimalCompletionStage(), deadline(), "") }
    }
}

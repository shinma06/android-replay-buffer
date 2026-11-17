package io.github.shinma06.replaybuffer.ide

import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.ReplayOperation
import io.github.shinma06.replaybuffer.core.SaveContent
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplaySaveAndTrimTest {
    @TempDir lateinit var root: Path

    private fun completed(id: String): SaveSnapshot {
        val directory = Files.createDirectory(root.resolve(id))
        Files.writeString(directory.resolve("video.mp4"), "video")
        Files.writeString(directory.resolve("replay.json"), "{}")
        return SaveSnapshot(SavePhase.COMPLETED, requestId = id, directory = directory)
    }

    @Test
    fun waitsForItsOwnCompletedAttemptEvenWhenAnotherRequestFinishesFirst() {
        val first = completed("first")
        val second = completed("second")
        val admitted = CompletableFuture<ReplayOperation>()
        val firstResult = CompletableFuture<SaveSnapshot>()
        val opened = mutableListOf<Path>()
        val observed = observeSaveForTrim(admitted, { true }, opened::add)
        assertTrue(opened.isEmpty())
        admitted.complete(ReplayOperation(true, requestId = first.requestId, saveResult = firstResult))
        assertFalse(observed.isDone)
        // Another save completing cannot substitute its directory for the pending request.
        val other = observeSaveForTrim(CompletableFuture.completedFuture(ReplayOperation(true,
            requestId = second.requestId, saveResult = CompletableFuture.completedFuture(second))), { true }, opened::add)
        other.get(10, TimeUnit.SECONDS)
        assertEquals(listOf(second.directory), opened)
        firstResult.complete(first)
        observed.get(10, TimeUnit.SECONDS)
        assertEquals(listOf(second.directory, first.directory), opened)
    }

    @Test
    fun rejectsFailedCancelledUnfinishedVideoLessAndUnrelatedResults() {
        val valid = completed("complete")
        val noVideo = completed("no-video").also { Files.delete(it.directory!!.resolve("video.mp4")) }
        val partial = completed("incomplete.partial").also { Files.delete(it.directory!!.resolve("replay.json")) }
        val directoryVideo = completed("directory-video").also {
            val directory = checkNotNull(it.directory)
            Files.delete(directory.resolve("video.mp4"))
            Files.createDirectory(directory.resolve("video.mp4"))
        }
        val cases = SavePhase.entries.filter { it != SavePhase.COMPLETED }.map { valid.copy(phase = it) } + listOf(
            valid.copy(directory = null), valid.copy(requestId = "unrelated"), valid.copy(requestId = null),
            valid.copy(captureMode = CaptureMode.LOG_ONLY), valid.copy(content = SaveContent.LOGS_ONLY),
            noVideo, partial, directoryVideo,
        )
        for (save in cases) {
            val opened = mutableListOf<Path>()
            val requestId = if (save.requestId == "unrelated" || save.requestId == null) valid.requestId else save.requestId
            observeSaveForTrim(CompletableFuture.completedFuture(ReplayOperation(true, requestId = requestId,
                saveResult = CompletableFuture.completedFuture(save))), { true }, opened::add).get(10, TimeUnit.SECONDS)
            assertTrue(opened.isEmpty(), save.toString())
        }
    }

    @Test
    fun completedVideoWithCoverageWarningsStillOpens() {
        val saved = completed("warnings").copy(missingKinds = listOf("video", "app_log"))
        val opened = mutableListOf<Path>()
        observeSaveForTrim(CompletableFuture.completedFuture(ReplayOperation(true, requestId = saved.requestId,
            saveResult = CompletableFuture.completedFuture(saved))), { true }, opened::add).get(10, TimeUnit.SECONDS)
        assertEquals(listOf(saved.directory), opened)
    }

    @Test
    fun rejectedOrDisposedRequestsNeverOpenOrAffectAnotherRequest() {
        val saved = completed("saved")
        val opened = mutableListOf<Path>()
        val completion = CompletableFuture<SaveSnapshot>()
        var alive = true
        val observed = observeSaveForTrim(CompletableFuture.completedFuture(ReplayOperation(true,
            requestId = saved.requestId, saveResult = completion)), { alive }, opened::add)
        observeSaveForTrim(CompletableFuture.completedFuture(ReplayOperation(false,
            requestId = saved.requestId, saveResult = CompletableFuture.completedFuture(saved))),
            { true }, opened::add).get(10, TimeUnit.SECONDS)
        observeSaveForTrim(CompletableFuture.completedFuture(ReplayOperation(true)),
            { true }, opened::add).get(10, TimeUnit.SECONDS)
        assertTrue(opened.isEmpty())
        alive = false
        completion.complete(saved)
        observed.get(10, TimeUnit.SECONDS)
        assertTrue(opened.isEmpty())
    }
}

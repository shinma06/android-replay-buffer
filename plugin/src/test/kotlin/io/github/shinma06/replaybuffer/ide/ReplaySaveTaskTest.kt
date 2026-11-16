package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.google.gson.JsonNull
import com.google.gson.JsonParser
import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.notification.NotificationDisplayType
import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.Notification
import com.intellij.notification.NotificationType
import com.intellij.notification.Notifications
import com.intellij.notification.NotificationsManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.client.ClientSession
import com.intellij.openapi.client.ClientSessionsManager
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.progress.EmptyProgressIndicator
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.progress.impl.CoreProgressManager
import com.intellij.openapi.util.Disposer
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import io.github.shinma06.replaybuffer.settings.ReplaySettingsState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import io.github.shinma06.replaybuffer.core.CaptureMode
import io.github.shinma06.replaybuffer.core.SaveContent
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.core.SaveFailure
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

class ReplaySaveTaskTest {
    @TempDir lateinit var root: Path

    private class Queue : CoreProgressManager() {
        lateinit var task: Task.Backgroundable
        var failure: Throwable? = null
        override fun run(task: Task) {
            this.task = task as Task.Backgroundable
            failure?.let { throw it }
        }
        fun finishCancelled() { finishTask(task, true, null) }
        override fun runProcessWithProgressAsynchronously(task: Task.Backgroundable, indicator: ProgressIndicator) {
            error("Saving must use the SDK's default visible progress, not an explicit indicator")
        }
    }

    private fun withQueue(
        onNotification: (Notification) -> Unit = {},
        uiWait: (Runnable, ModalityState) -> Unit = { runnable, _ -> runnable.run() },
        uiLater: Boolean = false,
        test: (Queue, MockProject) -> Unit,
    ) {
        val lifetime = Disposer.newDisposable()
        val app = object : MockApplication(lifetime) {
            override fun invokeLater(runnable: Runnable, state: ModalityState) { if (uiLater) runnable.run() }
            override fun invokeAndWait(runnable: Runnable, modalityState: ModalityState) { uiWait(runnable, modalityState) }
        }
        ApplicationManager.setApplication(app, lifetime)
        app.registerService(CoroutineScope::class.java, app.getCoroutineScope())
        app.registerService(ClientSessionsManager::class.java, ClientSessionsManager<ClientSession>(app.getCoroutineScope()))
        app.registerService(NotificationsManager::class.java, object : NotificationsManager() {
            override fun showNotification(notification: Notification, project: Project?) {}
            override fun expire(notification: Notification) {}
            override fun <T : Notification> getNotificationsOfType(klass: Class<T>, project: Project?): Array<T> = error("not queried")
        })
        app.registerService(NotificationGroupManager::class.java, object : NotificationGroupManager {
            override fun getNotificationGroup(groupId: String) =
                NotificationGroup.create(groupId, NotificationDisplayType.NONE, false, null, null, PluginId.getId("com.intellij"))
            override fun isGroupRegistered(groupId: String) = false
            override fun getRegisteredNotificationGroups() = emptyList<NotificationGroup>()
            override fun isRegisteredNotificationId(notificationId: String) = false
        })
        val queue = Queue()
        app.registerService(ProgressManager::class.java, queue)
        val project = MockProject(null, lifetime)
        project.messageBus.connect(lifetime).subscribe(Notifications.TOPIC, object : Notifications {
            override fun notify(notification: Notification) { onNotification(notification) }
        })
        try { test(queue, project) }
        finally { queue.dispose(); Disposer.dispose(lifetime) }
    }

    @Test
    fun bothSaveEntriesQueueVisibleCancellableBackgroundTasks() = withService { queue, service ->
        val cancel = AtomicBoolean(true)
        val png = service.exportReplayFrame(root.resolve("missing"), 0, cancel)
        assertEquals("静止画を保存しています", queue.task.title)
        assertQueued(queue.task)
        queue.task.run(EmptyProgressIndicator())
        assertNull(png.get(5, TimeUnit.SECONDS))

        val trim = service.trimReplay(root.resolve("missing"), 0, 1, cancel)
        assertEquals("動画を切り取っています", queue.task.title)
        assertQueued(queue.task)
        queue.task.run(EmptyProgressIndicator())
        assertEquals(ReplayProjectService.TrimRun(null), trim.get(5, TimeUnit.SECONDS))
        assertTrue(Files.list(root).use { it.findAny().isEmpty })
    }

    private fun assertQueued(task: Task.Backgroundable) {
        assertTrue(task.isCancellable)
        assertTrue(task.shouldStartInBackground())
        assertFalse(task.isModal)
        assertEquals("中止", task.cancelText)
    }

    @Test
    fun cancellationBeforeRunAndProjectDisposalCompleteBothFutures() = withService { queue, service ->
        val png = service.exportReplayFrame(root.resolve("missing"), 0)
        queue.finishCancelled()
        assertNull(png.get(5, TimeUnit.SECONDS))

        val trim = service.trimReplay(root.resolve("missing"), 0, 1)
        Disposer.dispose(queue.task.project!!)
        queue.finishCancelled()
        assertEquals(ReplayProjectService.TrimRun(null), trim.get(5, TimeUnit.SECONDS))
        assertTrue(service.exportReplayFrame(root, 0).isCompletedExceptionally)
        assertTrue(service.trimReplay(root, 0, 1).isCompletedExceptionally)
    }

    @Test
    fun disposedProjectStopsQueuedWorkBeforeOpeningSource() = withService { queue, service ->
        val png = service.exportReplayFrame(root.resolve("missing"), 0)
        val pngTask = queue.task
        val trim = service.trimReplay(root.resolve("missing"), 0, 1)
        Disposer.dispose(queue.task.project!!)
        pngTask.run(EmptyProgressIndicator())
        queue.task.run(EmptyProgressIndicator())
        assertNull(png.get(5, TimeUnit.SECONDS))
        assertEquals(ReplayProjectService.TrimRun(null), trim.get(5, TimeUnit.SECONDS))
    }

    @Test
    fun platformCancellationDuringWorkCompletesFutureAndIsRethrown() = withQueue { queue, project ->
        val done = CompletableFuture<Path?>()
        val failure = ProcessCanceledException()
        queueReplaySave(project, "保存しています", done, null) { throw failure }
        assertSame(failure, assertFailsWith<ProcessCanceledException> { queue.task.run(EmptyProgressIndicator()) })
        queue.task.onCancel()
        assertNull(done.get(5, TimeUnit.SECONDS))
    }

    @Test
    fun failuresBeforeAndDuringRunCompleteTheFuture() = withQueue { queue, project ->
        for (beforeRun in listOf(false, true)) {
            val done = CompletableFuture<Path?>()
            val failure = IOException("書き込みに失敗しました")
            queueReplaySave(project, "保存しています", done, null) { throw failure }
            if (beforeRun) queue.task.onThrowable(failure) else queue.task.run(EmptyProgressIndicator())
            assertSame(failure, assertFailsWith<ExecutionException> { done.get(5, TimeUnit.SECONDS) }.cause)
        }
    }

    @Test
    fun queueingFailureCompletesFutureAndPlatformCancellationIsRethrown() = withQueue { queue, project ->
        for (failure in listOf(IOException("開始できません"), ProcessCanceledException())) {
            val done = CompletableFuture<Path?>()
            queue.failure = failure
            if (failure is ProcessCanceledException) {
                assertSame(failure, assertFailsWith<ProcessCanceledException> {
                    queueReplaySave(project, "保存しています", done, null) { error("Must not run") }
                })
                assertNull(done.get(5, TimeUnit.SECONDS))
            } else {
                queueReplaySave(project, "保存しています", done, null) { error("Must not run") }
                assertSame(failure, assertFailsWith<ExecutionException> { done.get(5, TimeUnit.SECONDS) }.cause)
            }
        }
    }

    @Test
    fun aLateCancelDoesNotReplaceSuccessfulOutput() = withQueue { queue, project ->
        val output = root.resolve("completed.png")
        val done = CompletableFuture<Path?>()
        queueReplaySave(project, "保存しています", done, null) { Files.write(output, byteArrayOf(1, 2, 3)) }
        queue.task.run(EmptyProgressIndicator())
        queue.task.onCancel()
        queue.task.onFinished()
        assertEquals(output, done.get(5, TimeUnit.SECONDS))
        assertContentEquals(byteArrayOf(1, 2, 3), Files.readAllBytes(output))
    }

    @Test
    fun indicatorCancellationRetainsRealTrimPartialAndDoesNotModifySource() = cancelRunningTrim("indicator")

    @Test
    fun tokenCancellationRetainsRealTrimPartialAndDoesNotModifySource() = cancelRunningTrim("token")

    @Test
    fun projectDisposalDuringTrimCompletesFutureAndRetainsPartial() = cancelRunningTrim("project")

    @Test
    fun listingAliasesRetainTheSameTrimPartialForEveryCancellation() {
        for (reason in listOf("indicator", "token", "project")) {
            val parent = Files.createDirectory(root.resolve("parent-$reason"))
            val alias = parent.resolve(".")
            assertTrue(Files.isSameFile(parent, alias))
            assertFalse(parent == alias)
            cancelRunningTrim(reason, parent, alias)
        }
    }

    @Test
    fun indicatorCancellationDuringPngDecodeCompletesFutureWithoutOutput() = cancelRunningPng("indicator")

    @Test
    fun tokenCancellationDuringPngDecodeCompletesFutureWithoutOutput() = cancelRunningPng("token")

    @Test
    fun projectDisposalDuringPngDecodeCompletesFutureWithoutOutput() = cancelRunningPng("project")

    private fun source(parent: Path = root): Path {
        val name = "replay-20240101-000000-manual-feedface"
        val source = Files.createDirectory(parent.resolve(name))
        val names = listOf("README.md", "replay.json", "video.mp4", "video-source.h264", "video-frames.jsonl",
            "logcat-device.jsonl", "logcat-app.jsonl", "touches.jsonl")
        for (file in names) assertNotNull(javaClass.getResourceAsStream("/replay-trim-parity/$name/$file")).use {
            Files.copy(it, source.resolve(file))
        }
        return source
    }

    private fun snapshot(source: Path) = Files.list(source).use { files ->
        files.toList().associate { it.fileName.toString() to Files.readAllBytes(it) }
    }

    private fun unknownClockSource(parent: Path = root): Path {
        val source = source(parent)
        val index = source.resolve("video-frames.jsonl")
        Files.writeString(index, Files.readAllLines(index).joinToString("\n", postfix = "\n") { line ->
            JsonParser.parseString(line).asJsonObject.apply {
                addProperty("clock_alignment_known", false)
                for (key in listOf("t_ns", "display_t_ns", "display_t_end_ns", "confirmed_t_end_ns")) add(key, JsonNull.INSTANCE)
            }.toString()
        })
        val record = JsonParser.parseString(Files.readString(source.resolve("replay.json"))).asJsonObject
        record["files_sha256"].asJsonObject.addProperty("video-frames.jsonl",
            MessageDigest.getInstance("SHA-256").digest(Files.readAllBytes(index)).joinToString("") { "%02x".format(it) })
        Files.writeString(source.resolve("replay.json"), record.toString())
        return source
    }

    private class PendingNotification {
        val entered = CountDownLatch(1)
        val released = CountDownLatch(1)
        val runnable = AtomicReference<Runnable>()

        // Model LaterInvocator's cancellable wait and release of the queued notification on cancellation.
        fun invokeAndWait(action: Runnable, modality: ModalityState) {
            assertSame(ModalityState.nonModal(), modality)
            runnable.set(action)
            entered.countDown()
            try {
                while (!released.await(10, TimeUnit.MILLISECONDS)) ProgressManager.checkCanceled()
                dispatch()
            } finally { runnable.set(null) }
        }

        fun dispatch() { runnable.getAndSet(null)?.run() }
    }

    @Test
    fun tokenCancellationWhileWarningIsQueuedCompletesFutureAndTask() = pendingWarning("token")

    @Test
    fun serviceDisposalWhileWarningIsQueuedCompletesFutureAndTask() = pendingWarning("service")

    @Test
    fun indicatorCancellationWhileWarningIsQueuedCompletesFutureAndTask() = pendingWarning("indicator")

    @Test
    fun projectDisposalWhileWarningIsQueuedCompletesFutureAndTask() = pendingWarning("project")

    @Test
    fun deferredWarningStillPrecedesTrimStaging() = pendingWarning("none")

    @Test
    fun deferredWarningStillPrecedesTrimStagingWithAliasedListing() = pendingWarning("none", root.resolve("."))

    private fun pendingWarning(reason: String, listing: Path = root) {
        fun assertEntries(vararg expected: Path) {
            val remaining = Files.list(listing).use { it.toList().toMutableList() }
            assertEquals(expected.size, remaining.size)
            expected.forEach { path ->
                val index = remaining.indexOfFirst { Files.isSameFile(path, it) }
                assertTrue(index >= 0, "$path not found in $remaining")
                remaining.removeAt(index)
            }
        }
        val pending = PendingNotification()
        val notices = AtomicInteger()
        lateinit var source: Path
        withService(notify = { notification ->
            assertEquals(NotificationType.WARNING, notification.type)
            assertTrue(notification.content.contains("時計対応が不明"))
            assertEntries(source)
            notices.incrementAndGet()
        }, uiWait = pending::invokeAndWait) { queue, service ->
            source = unknownClockSource()
            val before = snapshot(source)
            val cancel = AtomicBoolean()
            val indicator = EmptyProgressIndicator()
            val done = service.trimReplay(source, 1, 500_000, cancel)
            val task = queue.task
            // The real GUI progress window is a child of the project and cancels on disposal.
            Disposer.register(task.project!!, Disposable { indicator.cancel() })
            val executor = Executors.newSingleThreadExecutor()
            val running = executor.submit {
                queue.runProcess({
                    task.run(indicator)
                    assertSame(indicator, queue.progressIndicator)
                }, indicator)
                assertNull(queue.progressIndicator)
            }
            try {
                assertTrue(pending.entered.await(5, TimeUnit.SECONDS))
                assertTrue(indicator.isRunning)
                assertFalse(done.isDone)
                assertEquals(0, notices.get())
                assertEntries(source)
                when (reason) {
                    "token" -> cancel.set(true)
                    "service" -> { service.dispose(); assertFalse(task.project!!.isDisposed) }
                    "indicator" -> indicator.cancel()
                    "project" -> Disposer.dispose(task.project!!)
                    "none" -> pending.released.countDown()
                    else -> error(reason)
                }
                val result = done.get(5, TimeUnit.SECONDS)
                running.get(5, TimeUnit.SECONDS)
                assertFalse(indicator.isRunning)
                assertNull(pending.runnable.get())
                if (reason == "none") {
                    assertFalse(indicator.isCanceled)
                    val output = assertNotNull(result.output)
                    assertEquals(1, notices.get())
                    assertEntries(source, output)
                } else {
                    assertEquals(1L, pending.released.count)
                    assertTrue(indicator.isCanceled)
                    assertEquals(ReplayProjectService.TrimRun(null), result)
                    queue.finishCancelled()
                    assertEquals(result, done.get(5, TimeUnit.SECONDS))
                    pending.dispatch()
                    assertEquals(0, notices.get())
                    assertEntries(source)
                }
                assertUnchanged(source, before)
            } finally {
                indicator.cancel()
                pending.released.countDown()
                executor.shutdown()
                assertTrue(executor.awaitTermination(5, TimeUnit.SECONDS))
            }
        }
    }

    @Test
    fun unknownClockNotificationPrecedesStagingAndPreservesCancellationAndSourceChangeGuards() {
        for (reason in listOf("none", "token", "indicator", "project", "source-change")) {
            val parent = Files.createDirectory(root.resolve(reason))
            val cancel = AtomicBoolean()
            lateinit var indicator: EmptyProgressIndicator
            var warnings = 0
            lateinit var source: Path
            lateinit var taskProject: com.intellij.openapi.project.Project
            withService(notify = { notification ->
                warnings++
                assertEquals(NotificationType.WARNING, notification.type)
                assertTrue(notification.content.contains("時計対応が不明"))
                assertEquals(listOf(source), Files.list(parent).use { it.toList() })
                when (reason) {
                    "token" -> cancel.set(true)
                    "indicator" -> indicator.cancel()
                    "project" -> Disposer.dispose(taskProject)
                    "source-change" -> Files.writeString(source.resolve("replay.json"),
                        JsonParser.parseString(Files.readString(source.resolve("replay.json"))).asJsonObject.apply {
                            addProperty("created_at", "2024-02-01T00:00:00Z")
                        }.toString())
                }
            }) { queue, service ->
                indicator = EmptyProgressIndicator()
                source = unknownClockSource(parent)
                val before = snapshot(source)
                val done = service.trimReplay(source, 1, 500_000, cancel)
                taskProject = queue.task.project!!
                queue.task.run(indicator)
                assertEquals(1, warnings)
                if (reason == "source-change") {
                    assertTrue(assertFailsWith<ExecutionException> { done.get(5, TimeUnit.SECONDS) }.cause!!.message!!.contains("事前確認後"))
                    assertEquals(listOf(source), Files.list(parent).use { it.toList() })
                    before.filterKeys { it != "replay.json" }.forEach { (name, bytes) ->
                        assertContentEquals(bytes, Files.readAllBytes(source.resolve(name)), name)
                    }
                } else {
                    val result = done.get(5, TimeUnit.SECONDS)
                    if (reason == "none") assertNotNull(result.output)
                    else {
                        assertEquals(ReplayProjectService.TrimRun(null), result)
                        assertEquals(listOf(source), Files.list(parent).use { it.toList() })
                    }
                    assertUnchanged(source, before)
                }
            }
        }
    }

    private fun assertUnchanged(source: Path, before: Map<String, ByteArray>) {
        val after = snapshot(source)
        assertEquals(before.keys, after.keys)
        val sha256 = MessageDigest.getInstance("SHA-256")
        for ((file, bytes) in before) {
            val current = assertNotNull(after[file], file)
            assertContentEquals(bytes, current, file)
            assertContentEquals(sha256.digest(bytes), sha256.digest(current), "$file SHA-256")
        }
    }

    private fun cancellationIndicator(queue: Queue, cancel: AtomicBoolean, reason: String, ready: () -> Boolean): ProgressIndicator {
        val indicator = EmptyProgressIndicator()
        return object : ProgressIndicator by indicator {
            override fun isCanceled(): Boolean {
                if (ready()) when (reason) {
                    "indicator" -> indicator.cancel()
                    "token" -> cancel.set(true)
                    "project" -> Disposer.dispose(queue.task.project!!)
                }
                return indicator.isCanceled
            }
        }
    }

    private fun cancelRunningPng(reason: String) = withService { queue, service ->
        val source = source()
        val before = snapshot(source)
        val cancel = AtomicBoolean()
        val done = service.exportReplayFrame(source, 700_000, cancel)
        var polls = 0
        queue.task.run(cancellationIndicator(queue, cancel, reason) { ++polls == 3 })
        queue.finishCancelled()
        assertEquals(3, polls)
        assertNull(done.get(5, TimeUnit.SECONDS))
        assertEquals(listOf(source), Files.list(root).use { it.toList() })
        assertUnchanged(source, before)
    }

    private fun cancelRunningTrim(reason: String, parent: Path = root, observedParent: Path = parent) = withService { queue, service ->
        val source = source(parent)
        val before = snapshot(source)
        val cancel = AtomicBoolean()
        val done = service.trimReplay(source, 1, 500_000, cancel)
        queue.task.run(cancellationIndicator(queue, cancel, reason) {
            Files.list(observedParent).use { files -> files.anyMatch { it.fileName.toString().endsWith(".partial") } }
        })
        val result = done.get(10, TimeUnit.SECONDS)
        val retained = assertNotNull(result.retainedPartial)
        queue.finishCancelled()
        assertEquals(result, done.get(5, TimeUnit.SECONDS))
        assertNull(result.output)
        assertTrue(Files.isDirectory(retained, NOFOLLOW_LINKS))
        assertTrue(retained.fileName.toString().endsWith(".partial"))
        assertTrue(Files.isSameFile(parent, retained.parent))
        assertFalse(Files.isSameFile(source, retained))
        val siblings = Files.list(observedParent).use { it.toList() }
        assertEquals(2, siblings.size)
        assertEquals(1, siblings.count { Files.isSameFile(source, it) })
        assertEquals(1, siblings.count { Files.isSameFile(retained, it) })
        assertUnchanged(source, before)
    }

    @Test
    fun cancelledTrimDoesNotReportRefreshCancellationAsSaveFailure() = assertTrimCompletionAfterRefreshFailure(
        ReplayProjectService.TrimRun(null, root.resolve("kept.partial")), ProcessCanceledException())

    @Test
    fun completedTrimKeepsSuccessWhenRefreshFailsAndCancellationArrivesLate() = assertTrimCompletionAfterRefreshFailure(
        ReplayProjectService.TrimRun(root.resolve("complete")), IOException("refresh failed"))

    private fun assertTrimCompletionAfterRefreshFailure(result: ReplayProjectService.TrimRun, refreshFailure: Throwable) {
        for (immediate in listOf(false, true)) {
            val done = CompletableFuture<ReplayProjectService.TrimRun>()
            val saved = mutableListOf<Path>()
            val failures = mutableListOf<Throwable>()
            var refreshes = 0
            if (immediate) done.complete(result)
            val observed = observeTrimSave(done, { refreshes++; throw refreshFailure }, saved::add, failures::add)
            if (!immediate) done.complete(result)
            assertSame(result, done.get(5, TimeUnit.SECONDS))
            assertEquals(1, refreshes)
            assertTrue(failures.isEmpty(), failures.toString())
            assertEquals(listOfNotNull(result.output), saved)
            assertSame(refreshFailure, assertFailsWith<ExecutionException> { observed.get(5, TimeUnit.SECONDS) }.cause)
            assertFalse(done.complete(ReplayProjectService.TrimRun(null)))
            assertFalse(done.completeExceptionally(IOException("late failure")))
            assertSame(result, done.get(5, TimeUnit.SECONDS))
        }
    }

    @Test
    fun failedTrimStillReportsTheOperationFailureOnce() {
        for (immediate in listOf(false, true)) {
            val done = CompletableFuture<ReplayProjectService.TrimRun>()
            val failure = SaveFailure(root.resolve("failed.partial"), "save failed", IOException("disk failure"))
            val failures = mutableListOf<Throwable>()
            if (immediate) done.completeExceptionally(failure)
            val observed = observeTrimSave(done, { error("Failed operation must not refresh") },
                { error("Failed operation must not report success") }, failures::add)
            if (!immediate) done.completeExceptionally(failure)
            observed.get(5, TimeUnit.SECONDS)
            val reported = failures.single()
            assertSame(failure, (reported as? java.util.concurrent.CompletionException)?.cause ?: reported)
            assertTrue(operationFailureContent(reported).contains(
                com.intellij.openapi.util.text.StringUtil.escapeXmlEntities(failure.partial.toString())))
            assertSame(failure, assertFailsWith<ExecutionException> { done.get(5, TimeUnit.SECONDS) }.cause)
            assertFalse(done.complete(ReplayProjectService.TrimRun(null)))
            assertEquals(1, failures.size)
        }
    }

    @Test
    fun logsOnlySaveNotificationDoesNotInventVideoWhenCurrentSettingsChange() = assertSavedVideoAction(CaptureMode.LOG_ONLY)

    @Test
    fun videoSaveNotificationKeepsVideoWhenCurrentSettingsChange() = assertSavedVideoAction(CaptureMode.VIDEO_AND_LOGS)

    @Test
    fun partialVideoNotificationKeepsVideoAndWarnings() = assertSavedVideoAction(CaptureMode.VIDEO_AND_LOGS,
        missing = listOf("video", "clock"))

    @Test
    fun partialLogsOnlyCaptureNotificationDoesNotInventVideo() = assertSavedVideoAction(CaptureMode.LOG_ONLY,
        missing = listOf("video", "clock"))

    @Test
    fun logsOnlyContentNotificationDoesNotExposeExistingVideo() = assertSavedVideoAction(CaptureMode.VIDEO_AND_LOGS,
        content = SaveContent.LOGS_ONLY, missing = listOf("video", "clock"))

    @Test
    fun absentVideoNotificationDoesNotOfferVideoEvenWithoutWarning() = assertSavedVideoAction(CaptureMode.VIDEO_AND_LOGS,
        videoPresent = false)

    @Test
    fun absentPartialVideoNotificationDoesNotOfferVideo() = assertSavedVideoAction(CaptureMode.VIDEO_AND_LOGS,
        missing = listOf("video"), videoPresent = false)

    @Test
    fun directoryNamedVideoDoesNotOfferVideo() = assertSavedVideoAction(CaptureMode.VIDEO_AND_LOGS,
        videoPresent = false, videoDirectory = true)

    private fun assertSavedVideoAction(savedMode: CaptureMode, content: SaveContent = SaveContent.ALL,
                                       missing: List<String> = emptyList(), videoPresent: Boolean = true,
                                       videoDirectory: Boolean = false) {
        val source = source()
        if (!videoPresent) Files.delete(source.resolve("video.mp4"))
        if (videoDirectory) Files.createDirectory(source.resolve("video.mp4"))
        val notifications = mutableListOf<Notification>()
        withService(notify = notifications::add, uiLater = true) { _, service ->
            val saved = SaveSnapshot(SavePhase.COMPLETED, "request", directory = source,
                captureMode = savedMode, content = content, missingKinds = missing)
            val project = service.javaClass.getDeclaredField("project").apply { isAccessible = true }.get(service) as Project
            val store = project.getService(ReplaySettingsStore::class.java)
            val notify = service.javaClass.getDeclaredMethod("notifySave", SaveSnapshot::class.java).apply { isAccessible = true }
            for (currentMode in CaptureMode.entries) {
                store.loadState(store.state.copy(captureMode = currentMode))
                notify.invoke(service, saved)
                val actions = notifications.last().actions.map { it.templatePresentation.text }
                assertEquals(savedMode.videoRequested && content == SaveContent.ALL && videoPresent, "動画を開く" in actions)
                assertTrue("フォルダを開く" in actions)
                assertTrue("パスをコピー" in actions)
                assertEquals(if (missing.isEmpty()) NotificationType.INFORMATION else NotificationType.WARNING, notifications.last().type)
                if (missing.isNotEmpty()) assertEquals("記録の不足・未確認: ${missing.map(::streamName).joinToString("、")}", notifications.last().content)
            }
        }
    }

    private fun withService(
        notify: (Notification) -> Unit = {},
        uiWait: (Runnable, ModalityState) -> Unit = { runnable, _ -> runnable.run() },
        uiLater: Boolean = false,
        test: (Queue, ReplayProjectService) -> Unit,
    ) = withQueue(notify, uiWait, uiLater) { queue, project ->
        project.registerService(ReplaySettingsStore::class.java, ReplaySettingsStore().apply {
            loadState(ReplaySettingsState(screenshotUsesLogcatDestination = false, screenshotDirectory = root.toString()))
        })
        project.registerService(GradleSyncState::class.java, Proxy.newProxyInstance(
            javaClass.classLoader, arrayOf(GradleSyncState::class.java),
        ) { _, method, args ->
            check(method.name == "subscribe")
            project.messageBus.connect(args[2] as Disposable)
        } as GradleSyncState)
        val service = ReplayProjectService(project, CoroutineScope(SupervisorJob().also { it.cancel() }))
        try { test(queue, service) } finally { service.dispose() }
    }
}

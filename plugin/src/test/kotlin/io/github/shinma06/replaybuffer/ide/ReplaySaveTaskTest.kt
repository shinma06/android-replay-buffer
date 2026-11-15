package io.github.shinma06.replaybuffer.ide

import com.android.tools.idea.gradle.project.sync.GradleSyncState
import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.notification.NotificationDisplayType
import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
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
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
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

    private fun withQueue(test: (Queue, MockProject) -> Unit) {
        val lifetime = Disposer.newDisposable()
        val app = object : MockApplication(lifetime) {
            override fun invokeLater(runnable: Runnable, state: ModalityState) {}
        }
        ApplicationManager.setApplication(app, lifetime)
        app.registerService(CoroutineScope::class.java, app.getCoroutineScope())
        app.registerService(ClientSessionsManager::class.java, ClientSessionsManager<ClientSession>(app.getCoroutineScope()))
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
    fun indicatorCancellationDuringPngDecodeCompletesFutureWithoutOutput() = cancelRunningPng("indicator")

    @Test
    fun tokenCancellationDuringPngDecodeCompletesFutureWithoutOutput() = cancelRunningPng("token")

    @Test
    fun projectDisposalDuringPngDecodeCompletesFutureWithoutOutput() = cancelRunningPng("project")

    private fun source(): Path {
        val name = "replay-20240101-000000-manual-feedface"
        val source = Files.createDirectory(root.resolve(name))
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

    private fun assertUnchanged(source: Path, before: Map<String, ByteArray>) {
        val after = snapshot(source)
        assertEquals(before.keys, after.keys)
        for ((file, bytes) in before) assertContentEquals(bytes, after[file], file)
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

    private fun cancelRunningTrim(reason: String) = withService { queue, service ->
        val source = source()
        val before = snapshot(source)
        val cancel = AtomicBoolean()
        val done = service.trimReplay(source, 1, 500_000, cancel)
        queue.task.run(cancellationIndicator(queue, cancel, reason) {
            Files.list(root).use { files -> files.anyMatch { it.fileName.toString().endsWith(".partial") } }
        })
        val result = done.get(10, TimeUnit.SECONDS)
        val retained = assertNotNull(result.retainedPartial)
        queue.finishCancelled()
        assertEquals(result, done.get(5, TimeUnit.SECONDS))
        assertNull(result.output)
        assertTrue(Files.isDirectory(retained))
        assertTrue(retained.fileName.toString().endsWith(".partial"))
        assertEquals(listOf(source, retained).toSet(), Files.list(root).use { it.toList().toSet() })
        assertUnchanged(source, before)
    }

    private fun withService(test: (Queue, ReplayProjectService) -> Unit) = withQueue { queue, project ->
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

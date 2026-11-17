package io.github.shinma06.replaybuffer.ui

import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.LightVirtualFile
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertSame

class ReplayUiActionTest {
    @TempDir lateinit var directory: Path

    @Test
    fun savedVideoChooserUsesTheConfiguredAbsoluteDirectoryWithoutCreatingIt() {
        val target = directory.resolve("リプレイ 保存先")
        val folder = object : LightVirtualFile("saved") {
            override fun isDirectory() = true
        }
        var lookedUp: Path? = null
        assertSame(folder, savedVideoChooserDirectory(target.toString()) { path ->
            lookedUp = path
            folder
        })
        assertEquals(target, lookedUp)
        assertFalse(Files.exists(target))
    }

    @Test
    fun unconfiguredOrMalformedChooserDirectoriesDoNotResolveTheWorkingDirectory() {
        for (value in listOf("", "relative", "\u0000")) {
            var lookups = 0
            assertNull(savedVideoChooserDirectory(value) { lookups++; null })
            assertEquals(0, lookups)
        }
    }

    @Test
    fun unavailableChooserDirectoriesUseTheIdeDefaultRatherThanAFileOrStaleNode() {
        val path = directory.toString()
        assertNull(savedVideoChooserDirectory(path) { null })
        assertNull(savedVideoChooserDirectory(path) { LightVirtualFile("video.mp4") })
        val stale = object : LightVirtualFile("stale") {
            override fun isDirectory() = true
        }.apply { setValid(false) }
        assertNull(savedVideoChooserDirectory(path) { stale })
        assertNull(savedVideoChooserDirectory(path) { throw SecurityException("unavailable") })
    }

    @Test
    fun chooserEntryUsesTheIdeDispatcherAndRechecksItsLifetime() {
        val lifetime = Disposer.newDisposable()
        val queued = ArrayDeque<Runnable>()
        val app = object : MockApplication(lifetime) {
            override fun invokeLater(runnable: Runnable, state: ModalityState) { queued.add(runnable) }
        }
        ApplicationManager.setApplication(app, lifetime)
        val project = MockProject(null, lifetime)
        try {
            var alive = true
            var calls = 0
            replayUiAction(project, { alive }) { calls++ }
            assertEquals(0, calls)
            assertEquals(1, queued.size)
            queued.removeFirst().run()
            assertEquals(1, calls)
            replayUiAction(project, { alive }) { calls++ }
            alive = false
            queued.removeFirst().run()
            assertEquals(1, calls)
        } finally {
            Disposer.dispose(lifetime)
        }
    }
}

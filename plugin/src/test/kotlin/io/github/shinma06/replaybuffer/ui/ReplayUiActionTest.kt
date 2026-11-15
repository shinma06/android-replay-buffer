package io.github.shinma06.replaybuffer.ui

import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.util.Disposer
import kotlin.test.Test
import kotlin.test.assertEquals

class ReplayUiActionTest {
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

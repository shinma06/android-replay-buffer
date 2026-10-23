package io.github.shinma06.replaybuffer.settings

import com.intellij.mock.MockApplication
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.CoroutineSupport
import com.intellij.openapi.application.impl.PlatformCoroutineSupport
import com.intellij.openapi.util.Disposer
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path
import javax.swing.SwingUtilities

class ReplayDestinationValidationTest {
    @TempDir lateinit var directory: Path

    @Test
    fun `newer input and disposed owner cancel delivery while the result returns on EDT`() = runBlocking {
        val lifetime = Disposer.newDisposable("destination validation test")
        val application = MockApplication(lifetime)
        ApplicationManager.setApplication(application, lifetime)
        application.registerService(CoroutineSupport::class.java, PlatformCoroutineSupport())
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val results = mutableListOf<DestinationValidation>()
        lateinit var old: Job
        lateinit var current: Job
        try {
            SwingUtilities.invokeAndWait {
                old = scope.checkDestination("relative/path") { results += it }
                old.cancel()
                current = scope.checkDestination(directory.toString()) {
                    assertTrue(SwingUtilities.isEventDispatchThread())
                    results += it
                }
            }
            withTimeout(10_000) { old.join(); current.join() }
            assertTrue(current.isCompleted && !current.isCancelled)
            assertEquals(listOf(DestinationValidation(directory.toString(), null)), results)
            SwingUtilities.invokeAndWait {
                current = scope.checkDestination("native-location", { throw IllegalStateException("unavailable native service") }) {
                    results += it
                }
            }
            withTimeout(10_000) { current.join() }
            assertEquals(DestinationValidation("native-location", LOGCAT_DESTINATION_UNAVAILABLE), results.last())
            SwingUtilities.invokeAndWait {
                current = scope.checkDestination("another/path") { results += it }
                scope.cancel()
            }
            withTimeout(10_000) { current.join() }
            assertTrue(current.isCancelled)
            assertEquals(2, results.size)
        } finally {
            scope.cancel()
            Disposer.dispose(lifetime)
        }
    }
}

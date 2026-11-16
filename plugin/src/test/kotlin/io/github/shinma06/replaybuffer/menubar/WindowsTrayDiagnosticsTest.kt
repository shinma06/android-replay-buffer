package io.github.shinma06.replaybuffer.menubar

import java.awt.IllegalComponentStateException
import java.io.PrintWriter
import java.io.StringWriter
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test

class WindowsTrayDiagnosticsTest {
    private val model = MenuBarMenu(MenuBarIcon.OFF, "test", emptyList())
    private val dispatch = MenuDispatch {}
    private fun rendered(failure: Throwable): String = StringWriter().also { failure.printStackTrace(PrintWriter(it)) }.toString()

    @Test
    fun `successful show and guarded action return unchanged without any logging`() {
        var warnings = 0
        val diagnostics = WindowsTrayDiagnostics { _, _ -> warnings++ }
        for (operation in WindowsTrayOperation.entries) {
            val value = Any()
            assertSame(value, diagnostics.guard(operation, { value }, { error("Unexpected recovery") }))
        }
        assertEquals(0, warnings)
    }

    @Test
    fun `both caught exception boundaries log before the unchanged recovery and return its result`() {
        for (operation in WindowsTrayOperation.entries) {
            val order = mutableListOf<String>()
            val warnings = mutableListOf<Pair<String, Throwable?>>()
            val original = IllegalComponentStateException("The dialog is decorated")
            val diagnostics = WindowsTrayDiagnostics { message, failure ->
                order += "log"
                warnings += message to failure
            }
            val recoveryResult = Any()
            assertSame(recoveryResult, diagnostics.guard(operation, { throw original }, { order += "recover"; recoveryResult }))
            assertEquals(listOf("log", "recover"), order)
            val (message, failure) = warnings.single()
            assertTrue(message.contains("operation=${operation.name}"))
            assertNotSame(original, failure)
            assertTrue(rendered(checkNotNull(failure)).contains("java.awt.IllegalComponentStateException: The dialog is decorated"))
        }
    }

    @Test
    fun `logger failure cannot prevent recovery or icon rejection`() {
        val diagnostics = WindowsTrayDiagnostics { _, _ -> throw IllegalStateException("logger unavailable") }
        assertEquals("same reason", diagnostics.guard(WindowsTrayOperation.SHOW, { error("failure") }, { "same reason" }))
        assertDoesNotThrow { diagnostics.openIfCurrent(false) { error("Replaced icon must not open") } }
    }

    @Test
    fun `Errors and failures in recovery still propagate without widening the original catch`() {
        var logged = 0
        val diagnostics = WindowsTrayDiagnostics { _, _ -> logged++ }
        val error = AssertionError("uncaught")
        assertSame(error, assertThrows(AssertionError::class.java) {
            diagnostics.guard(WindowsTrayOperation.GUARDED, { throw error }, { fail("Unexpected recovery") })
        })
        assertEquals(0, logged)
        val recoveryFailure = IllegalArgumentException("recovery failed")
        assertSame(recoveryFailure, assertThrows(IllegalArgumentException::class.java) {
            diagnostics.guard(WindowsTrayOperation.GUARDED, { error("caught") }, { throw recoveryFailure })
        })
        assertEquals(1, logged)
    }

    @Test
    fun `free text paths identities credentials and source metadata never reach the logged throwable tree`() {
        val privateText = "C:\\Users\\Alice Smith\\動画.txt /home/alice/private \\\\host\\share " +
            "alice@example.com token=secret-value https://example.com/private\nprivate next line"
        val cause = IllegalArgumentException(privateText)
        val original = IllegalStateException(privateText, cause).apply {
            addSuppressed(UnsupportedOperationException(privateText))
            stackTrace = arrayOf(StackTraceElement("private-loader", "private-module", "1", "java.awt.Dialog", "show", privateText, 42))
        }
        val originalFrames = original.stackTrace
        val safe = windowsTrayDiagnostic(original)
        val text = rendered(safe)
        for (privateValue in listOf("Alice", "alice", "動画", "secret-value", "example.com", "private", "host", "share"))
            assertFalse(text.contains(privateValue), privateValue)
        assertTrue(text.contains("[redacted: free-text message]"))
        for (type in listOf("java.lang.IllegalStateException", "java.lang.IllegalArgumentException", "java.lang.UnsupportedOperationException"))
            assertTrue(text.contains(type), type)
        assertEquals("java.awt.Dialog", safe.stackTrace.single().className)
        assertEquals("show", safe.stackTrace.single().methodName)
        assertEquals(42, safe.stackTrace.single().lineNumber)
        assertNull(safe.stackTrace.single().classLoaderName)
        assertNull(safe.stackTrace.single().moduleName)
        assertArrayEquals(originalFrames, original.stackTrace)
        assertEquals(privateText, original.message)
        assertSame(cause, original.cause)
    }

    @Test
    fun `known public message and missing message remain distinct from explicitly redacted messages`() {
        assertEquals("The dialog is decorated", windowsTrayDiagnostic(IllegalStateException("The dialog is decorated")).message)
        assertNull(windowsTrayDiagnostic(IllegalStateException()).message)
        assertEquals("[redacted: free-text message]", windowsTrayDiagnostic(IllegalStateException("The dialog is decorated /home/alice")).message)
    }

    @Test
    fun `cyclic or oversized exception graphs are bounded with explicit truncation markers`() {
        val first = IllegalStateException("The dialog is decorated")
        val second = IllegalArgumentException("The dialog is displayable")
        first.initCause(second)
        second.initCause(first)
        repeat(8) { first.addSuppressed(IllegalStateException("The dialog is decorated")) }
        first.stackTrace = Array(150) { StackTraceElement("java.awt.Dialog", "show", "Dialog.java", it) }
        val safe = windowsTrayDiagnostic(first)
        val text = rendered(safe)
        assertTrue(text.contains("[cycle omitted]"))
        assertTrue(text.contains("[22 omitted]"))
        assertTrue(text.contains("[4 suppressed omitted]"))
        assertEquals(129, safe.stackTrace.size)
        assertEquals(5, safe.suppressed.size)
    }

    @Test
    fun `current icon and successful open are silent while replacement rejects without opening`() {
        val reasons = mutableListOf<String>()
        val failures = mutableListOf<Throwable?>()
        val diagnostics = WindowsTrayDiagnostics { message, failure -> failures += failure; reasons += message }
        var opens = 0
        diagnostics.openIfCurrent(true) { opens++ }
        assertEquals(1, opens)
        assertTrue(reasons.isEmpty())
        diagnostics.openIfCurrent(false) { opens++ }
        assertEquals(1, opens)
        assertEquals(1, reasons.size)
        assertTrue(reasons.single().endsWith("reason=ICON_REPLACED"))
        assertEquals(listOf<Throwable?>(null), failures)
    }

    @Test
    fun `state rejections record fixed reasons without changing revision or precedence`() {
        val logs = mutableListOf<String>()
        val failures = mutableListOf<Throwable?>()
        val diagnostics = WindowsTrayDiagnostics { message, failure -> failures += failure; logs += message }
        val state = WindowsMenuState(diagnostics::rejected)
        assertNull(state.open(state.revision))
        assertTrue(logs.removeAt(0).endsWith("reason=NO_MENU"))
        state.update(model, dispatch)
        val revision = state.revision
        val session = state.open(revision)
        assertNotNull(session)
        assertTrue(logs.isEmpty())
        assertNull(state.open(revision))
        assertTrue(logs.removeAt(0).endsWith("reason=REVISION_CHANGED"))
        assertNull(state.open(state.revision))
        assertTrue(logs.removeAt(0).endsWith("reason=ALREADY_ACTIVE"))
        assertTrue(state.isActive(checkNotNull(session)))
        assertEquals(revision + 1, state.revision)
        state.close()
        val closedRevision = state.revision
        assertNull(state.open(revision))
        assertTrue(logs.removeAt(0).endsWith("reason=CLOSED"))
        assertEquals(closedRevision, state.revision)
        assertTrue(logs.isEmpty())
        assertEquals(List<Throwable?>(4) { null }, failures)
    }
}

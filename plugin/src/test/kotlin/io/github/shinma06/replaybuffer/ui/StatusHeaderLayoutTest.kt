package io.github.shinma06.replaybuffer.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.BasicText
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import io.github.shinma06.replaybuffer.core.ApplicationTarget
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.ReplayDevice
import io.github.shinma06.replaybuffer.core.ReplaySettings
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import java.nio.file.Path
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import io.github.shinma06.replaybuffer.settings.ReplaySettings as StoredSettings

class StatusHeaderLayoutTest {
    private val longName = "Pixel 9 Pro XL Fold " + "Very Long Device Name ".repeat(6)
    private val longPackage = "com.example." + "averyveryverylongsegment.".repeat(6) + "app"

    private fun longHeader(): String {
        val snapshot = ReplaySnapshot(1, 1, true, CaptureState.CAPTURING,
            ReplaySettings(replaySeconds = 30, application = ApplicationTarget(longPackage)), 1,
            device = ReplayDevice("emulator-5554", longName, DeviceKind.EMULATOR, true), sequenceId = "session",
            windowEndNs = 1_000_000_000L, canSave = true)
        val model = toolWindowModel(ToolWindowInput(snapshot = snapshot, message = null, initializationError = null,
            busy = false, enabled = true, settings = StoredSettings(retentionSeconds = 30), adb = Path.of("/sdk/adb"),
            adbReason = null, screenshotPackage = longPackage, screenshotDisabledReason = null, screenshotBusy = false))
        return "${model.header.symbol} ${model.header.text}"
    }

    @Test
    fun `capture switch keeps its width beside a long headline in a narrow tool window`() {
        val header = longHeader()
        assertTrue(header.contains(longName) && header.contains(longPackage))
        for (width in listOf(160, 240, 320)) {
            var heading = Rect.Zero
            var toggle = Rect.Zero
            val scene = ImageComposeScene(width, 40, Density(1f)) {
                StatusHeaderRow(
                    heading = {
                        BasicText(header, Modifier.onGloballyPositioned { heading = it.boundsInRoot() },
                            maxLines = 1, overflow = TextOverflow.Ellipsis)
                    },
                    toggle = { Box(Modifier.size(56.dp, 20.dp).onGloballyPositioned { toggle = it.boundsInRoot() }) },
                )
            }
            try {
                scene.render()
            } finally {
                scene.close()
            }
            assertEquals(56f, toggle.width, "switch keeps its own width at $width px")
            assertTrue(toggle.left >= 0f && toggle.right <= width, "switch is inside the window at $width px: $toggle")
            assertTrue(heading.right <= toggle.left, "headline ends before the switch at $width px: $heading / $toggle")
            assertTrue(heading.width > 0f, "headline still shows at $width px")
        }
    }
}

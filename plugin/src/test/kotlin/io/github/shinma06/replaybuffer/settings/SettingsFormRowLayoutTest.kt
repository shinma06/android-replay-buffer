package io.github.shinma06.replaybuffer.settings

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.BasicText
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class SettingsFormRowLayoutTest {
    private class Bounds(val label: Rect, val field: Rect, val button: Rect)

    private fun layout(width: Int): Bounds {
        var label = Rect.Zero
        var field = Rect.Zero
        var button = Rect.Zero
        val scene = ImageComposeScene(width, 40, Density(1f)) {
            FormRow({ BasicText("動画・ログの保存先:", it.onGloballyPositioned { c -> label = c.boundsInRoot() }) }) {
                Box(flexibleWidth(PATH_FIELD_WIDTH).size(height = 24.dp, width = PATH_FIELD_WIDTH)
                    .onGloballyPositioned { field = it.boundsInRoot() })
                Box(Modifier.size(64.dp, 24.dp).onGloballyPositioned { button = it.boundsInRoot() })
            }
        }
        try {
            scene.render()
        } finally {
            scene.close()
        }
        return Bounds(label, field, button)
    }

    @Test
    fun `folder button stays reachable when the settings page is narrower than the path field`() {
        for (width in listOf(320, 420, 520, 599)) {
            val row = layout(width)
            assertEquals(64f, row.button.width, "参照… keeps its own width at $width px")
            assertTrue(row.button.left >= 0f && row.button.right <= width, "参照… is inside the page at $width px: ${row.button}")
            assertTrue(row.field.width > 0f && row.field.right <= row.button.left, "field yields to 参照… at $width px: ${row.field}")
            assertTrue(row.label.right <= row.field.left, "label stays before the field at $width px")
        }
    }

    @Test
    fun `path field keeps its preferred width when the page is wide`() {
        val row = layout(900)
        assertEquals(PATH_FIELD_WIDTH.value, row.field.width)
        assertEquals(row.field.right + 8f, row.button.left, "参照… follows the field instead of the page edge")
    }
}

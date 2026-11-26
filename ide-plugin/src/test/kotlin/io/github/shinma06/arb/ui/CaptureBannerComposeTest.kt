package io.github.shinma06.arb.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.neverEqualPolicy
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.dp
import io.github.shinma06.arb.state.CaptureBanner
import io.github.shinma06.arb.state.CaptureBannerKind
import org.jetbrains.jewel.foundation.BorderColors
import org.jetbrains.jewel.foundation.GlobalColors
import org.jetbrains.jewel.foundation.LocalGlobalColors
import org.jetbrains.jewel.foundation.OutlineColors
import org.jetbrains.jewel.foundation.TextColors
import org.jetbrains.jewel.foundation.theme.LocalContentColor
import org.jetbrains.jewel.foundation.theme.OverrideDarkMode
import org.jetbrains.jewel.foundation.theme.LocalTextStyle
import org.jetbrains.skia.Bitmap
import org.junit.jupiter.api.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

@OptIn(ExperimentalComposeUiApi::class)
class CaptureBannerComposeTest {
    @Test
    fun accentFollowsContentHeightWithAnEighteenDpMinimum() {
        for (height in listOf(8, 32, 100)) {
            val scene = ImageComposeScene(200, 200) {
                Box {
                    ArbSoftBanner(Color.White, Color.Red) {
                        Spacer(Modifier.height(height.dp))
                    }
                }
            }
            try {
                scene.render().use { image ->
                    Bitmap.makeFromImage(image).use { pixels ->
                        val red = (0 until 200).filter { pixels.getColor(11, it) == Color.Red.toArgb() }
                        // The outermost pixels are antialiased by the 2dp rounded ends.
                        assertTrue(red.size in (height.coerceAtLeast(18) - 2)..height.coerceAtLeast(18), red.toString())
                    }
                }
            } finally {
                scene.close()
            }
        }
    }

    @Test
    fun expandedTraceSurvivesPollingAndCopiesTheOriginal() {
        val raw = "scrcpy stderr: failed | at java.lang.Thread.run()  "
        val banner = mutableStateOf(CaptureBanner(CaptureBannerKind.CONFIG_WARNING, "音声", "failed", raw), neverEqualPolicy())
        var copied = ""
        val ink = Color.Black
        val colors = GlobalColors(
            BorderColors(ink, ink, ink), OutlineColors(ink, ink, ink, ink, ink),
            TextColors(ink, ink, ink, ink, ink, ink, ink), Color.White, Color.White,
        )
        val scene = ImageComposeScene(320, 300) {
            CompositionLocalProvider(
                LocalGlobalColors provides colors,
                LocalContentColor provides ink, LocalTextStyle provides TextStyle.Default,
            ) {
                OverrideDarkMode(false) { Box { CaptureAlertBanner(banner.value) { copied = it } } }
            }
        }
        fun nodes(node: SemanticsNode): List<SemanticsNode> = listOf(node) + node.children.flatMap(::nodes)
        fun visibleNodes(): List<SemanticsNode> {
            scene.render().close()
            return nodes(scene.semanticsOwners.single().rootSemanticsNode)
        }
        fun shows(text: String) = visibleNodes().any { node ->
            node.config.getOrNull(SemanticsProperties.Text).orEmpty().any { it.text == text }
        }
        fun click(label: String) {
            val node = visibleNodes().single { label in it.config.getOrNull(SemanticsProperties.ContentDescription).orEmpty() &&
                it.config.contains(SemanticsActions.OnClick) }
            assertTrue(node.config[SemanticsActions.OnClick].action!!.invoke())
        }
        try {
            assertFalse(shows(raw))
            click("詳細を表示")
            assertTrue(shows(raw))
            banner.value = banner.value.copy()
            assertTrue(shows(raw))
            click("詳細をコピー")
            assertEquals(raw, copied)
            click("詳細を隠す")
            assertFalse(shows(raw))
            banner.value = banner.value.copy(technicalDetail = null)
            assertFalse(shows("詳細を表示"))
        } finally {
            scene.close()
        }
    }
}

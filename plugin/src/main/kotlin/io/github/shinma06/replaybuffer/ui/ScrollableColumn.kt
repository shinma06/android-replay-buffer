package io.github.shinma06.replaybuffer.ui

import androidx.compose.foundation.VerticalScrollbar
import androidx.compose.foundation.defaultScrollbarStyle
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.rememberScrollbarAdapter
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.theme.scrollbarStyle

@Composable
internal fun ScrollableColumn(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val scrollState = rememberScrollState()
    val adapter = rememberScrollbarAdapter(scrollState)
    val jewelScrollbarStyle = JewelTheme.scrollbarStyle
    val scrollbarStyle = remember(jewelScrollbarStyle) {
        defaultScrollbarStyle().copy(
            minimalHeight = jewelScrollbarStyle.metrics.minThumbLength,
            thickness = jewelScrollbarStyle.scrollbarVisibility.trackThickness,
            shape = RoundedCornerShape(jewelScrollbarStyle.metrics.thumbCornerSize),
            hoverDurationMillis = jewelScrollbarStyle.scrollbarVisibility.lingerDuration.inWholeMilliseconds.toInt(),
            unhoverColor = jewelScrollbarStyle.colors.thumbBackground,
            hoverColor = jewelScrollbarStyle.colors.thumbBackgroundActive,
        )
    }
    val interactionSource = remember { MutableInteractionSource() }

    Box(modifier) {
        Column(Modifier.fillMaxSize().verticalScroll(scrollState), content = content)
        VerticalScrollbar(
            adapter = adapter,
            modifier = Modifier.align(Alignment.CenterEnd).fillMaxHeight(),
            reverseLayout = false,
            style = scrollbarStyle,
            interactionSource = interactionSource,
        )
    }
}

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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.component.styling.ScrollbarVisibility
import org.jetbrains.jewel.ui.theme.scrollbarStyle

internal enum class ScrollbarVisibilityPolicy {
    AlwaysVisible,
    WhenScrolling,
}

internal enum class ScrollbarActivity {
    Hidden,
    Scrolling,
    Lingering,
}

internal fun scrollbarActivityAfterScrollChange(
    activity: ScrollbarActivity,
    isScrollInProgress: Boolean,
): ScrollbarActivity = when {
    isScrollInProgress -> ScrollbarActivity.Scrolling
    activity == ScrollbarActivity.Scrolling -> ScrollbarActivity.Lingering
    else -> activity
}

internal fun scrollbarActivityAfterLinger(activity: ScrollbarActivity): ScrollbarActivity =
    if (activity == ScrollbarActivity.Lingering) ScrollbarActivity.Hidden else activity

internal fun isScrollbarVisible(
    policy: ScrollbarVisibilityPolicy,
    isScrollInProgress: Boolean,
    activity: ScrollbarActivity,
): Boolean = policy == ScrollbarVisibilityPolicy.AlwaysVisible ||
    isScrollInProgress ||
    activity != ScrollbarActivity.Hidden

internal data class ScrollbarThumbColorPair(
    val idle: Color,
    val hover: Color,
)

internal fun scrollbarThumbColorPair(
    policy: ScrollbarVisibilityPolicy,
    scrollbarVisible: Boolean,
    thumbOpaqueBackground: Color,
    thumbBackgroundActive: Color,
): ScrollbarThumbColorPair = ScrollbarThumbColorPair(
    idle = when {
        policy == ScrollbarVisibilityPolicy.AlwaysVisible -> thumbOpaqueBackground
        scrollbarVisible -> thumbBackgroundActive
        else -> Color.Transparent
    },
    hover = thumbBackgroundActive,
)

@Composable
internal fun ScrollableColumn(
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val scrollState = rememberScrollState()
    val adapter = rememberScrollbarAdapter(scrollState)
    val jewelScrollbarStyle = JewelTheme.scrollbarStyle
    val jewelVisibility = jewelScrollbarStyle.scrollbarVisibility
    val visibilityPolicy = if (jewelVisibility is ScrollbarVisibility.AlwaysVisible) {
        ScrollbarVisibilityPolicy.AlwaysVisible
    } else {
        ScrollbarVisibilityPolicy.WhenScrolling
    }
    val lingerMillis = jewelVisibility.lingerDuration.inWholeMilliseconds
    var scrollbarActivity by remember(visibilityPolicy) { mutableStateOf(ScrollbarActivity.Hidden) }
    val isScrollInProgress = scrollState.isScrollInProgress

    LaunchedEffect(scrollState, visibilityPolicy, lingerMillis) {
        if (visibilityPolicy == ScrollbarVisibilityPolicy.WhenScrolling) {
            snapshotFlow { scrollState.isScrollInProgress }.collectLatest { scrolling ->
                scrollbarActivity = scrollbarActivityAfterScrollChange(scrollbarActivity, scrolling)
                if (!scrolling && scrollbarActivity == ScrollbarActivity.Lingering) {
                    delay(lingerMillis)
                    scrollbarActivity = scrollbarActivityAfterLinger(scrollbarActivity)
                }
            }
        }
    }

    val scrollbarVisible = isScrollbarVisible(visibilityPolicy, isScrollInProgress, scrollbarActivity)
    val scrollbarStyle = remember(jewelScrollbarStyle, visibilityPolicy, scrollbarVisible) {
        val thumbBackgroundActive = jewelScrollbarStyle.colors.thumbBackgroundActive
        val thumbColors = scrollbarThumbColorPair(
            visibilityPolicy,
            scrollbarVisible,
            jewelScrollbarStyle.colors.thumbOpaqueBackground,
            thumbBackgroundActive,
        )
        defaultScrollbarStyle().copy(
            minimalHeight = jewelScrollbarStyle.metrics.minThumbLength,
            thickness = jewelVisibility.trackThickness,
            shape = RoundedCornerShape(jewelScrollbarStyle.metrics.thumbCornerSize),
            hoverDurationMillis = jewelVisibility.thumbColorAnimationDuration.inWholeMilliseconds.toInt(),
            unhoverColor = thumbColors.idle,
            hoverColor = thumbColors.hover,
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
